//! Matching a set of rules against a term.
//!
//! Design: `spec/10-backend.md` section 10.2 and `spec/optimizer/13-rewrite-rules.md`. The rules
//! themselves are rule files, one per rule set, and the automaton they compile into is generated
//! by `rucc-rules` when the crate that owns the file is built. What is here is the walk over
//! that automaton, which is the same walk for every rule set and is written once.
//!
//! # Why this is at the bottom of the stack
//!
//! Two crates match with a generated table and neither can see the other. `rucc-codegen` lowers
//! IR to machine terms and `rucc-opt` rewrites IR to IR, and a lowering and a rewrite are the
//! same claim about two terms, so they are the same trie and the same walk. Putting the walk
//! here rather than in either of them is what keeps that true rather than merely intended, and
//! it costs nothing: none of this knows what an instruction is, what a value is, or what C is.
//!
//! # What a subject is
//!
//! A rule matches a term, and the compiler does not have terms: it has a function full of
//! instructions, and what a pattern is about is one of them and whatever it was computed from.
//! So the walk is written against [`Subject`], which is the three questions the automaton asks
//! of whatever it is matching, and a caller answers them out of the IR without building a term
//! to be thrown away. A test can answer them out of anything at all, which is what the tests at
//! the bottom of this file do.
//!
//! # What a match gives back
//!
//! The rule that fired and what its pattern bound, in the order the pattern binds it. The
//! bindings are positions rather than names because that is what the walk has, and the rule
//! carries the names for anything that has to say what it did. Building the replacement out of
//! [`Piece`] belongs to the caller rather than to this file, because what a replacement becomes
//! is a machine instruction in one crate and an IR instruction in the other, and this module is
//! about matching.
//!
//! # A name written twice
//!
//! A pattern may write one name in two places, which is how `x & x` is said. The second place
//! becomes [`Test::Same`] rather than a hole, and it asks the subject whether the two are the
//! same thing rather than comparing nodes, because a node is a place and two places can hold one
//! value. It is a concrete test, so it is tried before the wildcard for the same reason every
//! other test is: a rule about one value in both operands is more specific than a rule about any
//! two.
//!
//! # Order
//!
//! At every node the concrete tests are tried before the branch that takes anything, so a rule
//! naming an operand is tried before a rule taking whatever is there. That is the maximal munch
//! `spec/10-backend.md` asks for, and it falls out of the shape of the trie rather than being
//! sorted for. Among rules that are equally specific the first one written wins.
//!
//! A guard is part of deciding whether a rule fires, so a rule whose guard is false is a rule
//! that did not match, and the walk carries on looking rather than giving up. What that costs is
//! the search from where the guard failed, which is the price of a guard being allowed to be
//! about the values rather than only about the shape.

/// The bits of a term the automaton asks about.
///
/// A node is whatever the thing doing the matching calls one of its terms: an IR value, an index
/// into an arena, a pointer. It has to be cheap to copy because the walk keeps a stack of them.
pub trait Subject {
    /// What this subject calls one of its terms.
    type Node: Copy;

    /// The head of a term and how many arguments it has, or nothing if the term is not an
    /// application. An IR instruction answers with its opcode and its width, spelled the way the
    /// rule file spells it.
    fn head(&self, node: Self::Node) -> Option<(&str, usize)>;

    /// One argument of a term, counted from zero. Only ever asked for an argument the answer to
    /// [`Subject::head`] said was there.
    fn arg(&self, node: Self::Node, index: usize) -> Self::Node;

    /// The value of a term that is a constant, or nothing if it is not one. This is what a
    /// pattern matching a literal is asking, and what a guard reads.
    fn int(&self, node: Self::Node) -> Option<i128>;

    /// Whether two terms are the same thing, which is what a pattern that writes one name in two
    /// places is asking.
    ///
    /// This is a question for the subject rather than something the walk can answer by comparing
    /// nodes, because a node is a place and two places can hold one value. In
    /// `(and.i32 (value.i32 x) (value.i32 x))` the two operands are operand zero and operand
    /// one, which are different places, and what the rule wants to know is whether the same
    /// value is in both. A subject that cannot tell may answer `false`, which costs the rule a
    /// match it could have had and never gives it one it should not.
    fn same(&self, a: Self::Node, b: Self::Node) -> bool;
}

/// One test on one subterm.
#[derive(Debug)]
pub enum Test {
    /// The subterm has to be this head applied to this many arguments.
    App {
        /// The name in head position.
        head: &'static str,
        /// How many arguments it takes.
        arity: usize,
    },
    /// The subterm has to be this constant.
    Int(i128),
    /// The subterm has to be the same thing as a binding this pattern already made, named by
    /// which binding it is. A pattern writes one where it writes a name for the second time, so
    /// this is how `x & x` is told apart from `x & y`.
    Same(usize),
}

/// One node of the trie over the patterns.
#[derive(Debug)]
pub struct Node {
    /// The tests to try, in the order the rules were written, before the wildcard.
    pub tests: &'static [(Test, u32)],
    /// The branch that takes anything, and the name the first rule to reach it gave that hole.
    pub wildcard: Option<(&'static str, u32)>,
    /// The rule that ends here, if one does.
    pub accept: Option<u32>,
}

/// One piece of a replacement, in the pre-order that builds it.
#[derive(Debug)]
pub enum Piece {
    /// Whatever the pattern bound at this position.
    Var {
        /// The name the rule gave it, for anything that has to say what it did.
        name: &'static str,
        /// Which binding of the match it is.
        index: usize,
    },
    /// A constant written in the rule.
    Int(i128),
    /// A term the rule writes, which is an instruction once the caller has built it.
    App {
        /// The name in head position.
        head: &'static str,
        /// How many arguments it takes.
        arity: usize,
    },
}

/// A condition on the constants a pattern matched.
///
/// It is handed one entry per binding, holding the value of that binding when it has one. A
/// guard about a binding that is not a constant is false, which is how a rule about a number
/// declines an operand that is a register.
pub type Guard = fn(&[Option<i128>]) -> bool;

/// One rule, as much of it as matching needs.
#[derive(Debug)]
pub struct Rule {
    /// The pattern as it is written in the rule file, for diagnostics and for tests.
    pub pattern: &'static str,
    /// What to put in the matched term's place, flattened into pre-order.
    pub replacement: &'static [Piece],
    /// The condition on the match, if the rule has one.
    pub guard: Option<Guard>,
    /// The line of the rule file this rule starts on.
    pub line: u32,
}

impl Rule {
    /// The head of the replacement, which is what this rule writes.
    #[must_use]
    pub fn head(&self) -> Option<&'static str> {
        match self.replacement.first() {
            Some(Piece::App { head, .. }) => Some(head),
            _ => None,
        }
    }
}

/// A set of rules, as an automaton over their patterns.
#[derive(Debug)]
pub struct Table {
    /// The rule file this was built from, so that anything said about a rule can name a file
    /// somebody can open.
    pub source: &'static str,
    /// The trie. Node zero is the root.
    pub nodes: &'static [Node],
    /// The rules, in the order the file writes them.
    pub rules: &'static [Rule],
}

/// What a successful match found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match<N> {
    /// Which rule of the table fired.
    pub rule: usize,
    /// What the pattern bound, in the order it binds it.
    pub bindings: Vec<N>,
}

impl Table {
    /// The rule that fires on this term, and what it bound.
    ///
    /// The term is matched as a whole. Finding the terms in a function worth matching is the
    /// caller's job and not this one's.
    #[must_use]
    pub fn find<S: Subject>(&self, subject: &S, term: S::Node) -> Option<Match<S::Node>> {
        let mut bindings = Vec::new();
        let rule = self.run(subject, 0, vec![term], &mut bindings)?;
        Some(Match { rule, bindings })
    }

    /// The rule a match found, which is the one thing every caller wants out of it.
    #[must_use]
    pub fn rule<N>(&self, found: &Match<N>) -> &Rule {
        &self.rules[found.rule]
    }

    /// Walk the trie and the subject together.
    ///
    /// `left` is the subterms still to be matched, innermost last, so that popping gives the
    /// pre-order the patterns were flattened in.
    fn run<S: Subject>(
        &self,
        subject: &S,
        at: usize,
        mut left: Vec<S::Node>,
        bindings: &mut Vec<S::Node>,
    ) -> Option<usize> {
        let Some(term) = left.pop() else {
            return self.accept(subject, at, bindings);
        };
        let node = &self.nodes[at];
        let head = subject.head(term);

        for (test, next) in node.tests {
            let matched = match test {
                Test::Int(want) => subject.int(term) == Some(*want),
                Test::App { head: want, arity } => {
                    head.is_some_and(|(have, count)| have == *want && count == *arity)
                }
                // The binding is always there, because a pattern only writes a name for the
                // second time after it has written it once and the trie keeps that order.
                Test::Same(index) => {
                    bindings.get(*index).is_some_and(|&bound| subject.same(bound, term))
                }
            };
            if !matched {
                continue;
            }
            let mut deeper = left.clone();
            if let Some((_, arity)) = head {
                for index in (0..arity).rev() {
                    deeper.push(subject.arg(term, index));
                }
            }
            let depth = bindings.len();
            if let Some(rule) = self.run(subject, *next as usize, deeper, bindings) {
                return Some(rule);
            }
            bindings.truncate(depth);
        }

        // The wildcard is last, which is the whole of what specificity order means here.
        let (_, next) = node.wildcard.as_ref()?;
        let depth = bindings.len();
        bindings.push(term);
        if let Some(rule) = self.run(subject, *next as usize, left, bindings) {
            return Some(rule);
        }
        bindings.truncate(depth);
        None
    }

    /// The rule that ends at this node, if one does and if its guard holds.
    fn accept<S: Subject>(&self, subject: &S, at: usize, bindings: &[S::Node]) -> Option<usize> {
        let rule = self.nodes[at].accept? as usize;
        if let Some(guard) = self.rules[rule].guard {
            // The values are collected here rather than as the bindings are made, because most
            // rules have no guard and would pay for it every time.
            let values: Vec<Option<i128>> =
                bindings.iter().map(|&node| subject.int(node)).collect();
            if !guard(&values) {
                return None;
            }
        }
        Some(rule)
    }
}

#[cfg(test)]
mod tests {
    use super::{Match, Node, Piece, Rule, Subject, Table, Test};

    /// A term, in the only shape a test needs: a flat arena, because that is the shape the IR
    /// has and answering the questions out of one is what the callers will be doing.
    #[derive(Debug)]
    enum Held {
        Int(i128),
        App(String, Vec<usize>),
    }

    #[derive(Debug, Default)]
    struct Terms {
        nodes: Vec<Held>,
    }

    impl Terms {
        fn constant(&mut self, value: i128) -> usize {
            self.nodes.push(Held::Int(value));
            self.nodes.len() - 1
        }

        fn app(&mut self, head: &str, args: &[usize]) -> usize {
            self.nodes.push(Held::App(head.to_owned(), args.to_vec()));
            self.nodes.len() - 1
        }
    }

    impl Subject for Terms {
        type Node = usize;

        fn head(&self, node: usize) -> Option<(&str, usize)> {
            match &self.nodes[node] {
                Held::App(head, args) => Some((head.as_str(), args.len())),
                Held::Int(_) => None,
            }
        }

        fn arg(&self, node: usize, index: usize) -> usize {
            match &self.nodes[node] {
                Held::App(_, args) => args[index],
                Held::Int(_) => unreachable!("a constant has no arguments"),
            }
        }

        fn int(&self, node: usize) -> Option<i128> {
            match self.nodes[node] {
                Held::Int(value) => Some(value),
                Held::App(..) => None,
            }
        }

        // An index into the arena is the identity of a term here, so two places are the same
        // thing when they point at the same entry. A subject over the IR answers this out of the
        // value each place holds instead, which is the same question asked of a different shape.
        fn same(&self, a: usize, b: usize) -> bool {
            a == b
        }
    }

    /// A table written by hand, in the shape `rucc-rules` emits.
    ///
    /// Two rules over `(add x k)`: the first wants the constant to be zero and the second takes
    /// any constant that is not negative. That is enough to exercise everything the walk does,
    /// which is a concrete test before a wildcard, a guard that can refuse, and the search
    /// carrying on after it does. A third rule, `(and x x)`, is the one that writes a name
    /// twice.
    static NODES: &[Node] = &[
        // 0, the root.
        Node {
            tests: &[
                (Test::App { head: "add", arity: 2 }, 1),
                (Test::App { head: "and", arity: 2 }, 5),
            ],
            wildcard: None,
            accept: None,
        },
        // 1, the first operand.
        Node { tests: &[], wildcard: Some(("x", 2)), accept: None },
        // 2, the second operand.
        Node { tests: &[(Test::Int(0), 3)], wildcard: Some(("k", 4)), accept: None },
        // 3, an addition of zero.
        Node { tests: &[], wildcard: None, accept: Some(0) },
        // 4, an addition of anything, if the guard holds.
        Node { tests: &[], wildcard: None, accept: Some(1) },
        // 5, the first operand of the conjunction, which is the one that binds.
        Node { tests: &[], wildcard: Some(("x", 6)), accept: None },
        // 6, the second operand, which has to be what the first one bound.
        Node { tests: &[(Test::Same(0), 7)], wildcard: None, accept: None },
        // 7, a conjunction of one thing with itself.
        Node { tests: &[], wildcard: None, accept: Some(2) },
    ];

    fn not_negative(bound: &[Option<i128>]) -> bool {
        let Some(Some(k)) = bound.get(1).copied() else { return false };
        k >= 0
    }

    static RULES: &[Rule] = &[
        Rule {
            pattern: "(add x 0)",
            replacement: &[Piece::Var { name: "x", index: 0 }],
            guard: None,
            line: 1,
        },
        Rule {
            pattern: "(add x k)",
            replacement: &[
                Piece::App { head: "add_immediate", arity: 2 },
                Piece::Var { name: "x", index: 0 },
                Piece::Var { name: "k", index: 1 },
            ],
            guard: Some(not_negative),
            line: 2,
        },
        Rule {
            pattern: "(and x x)",
            replacement: &[Piece::Var { name: "x", index: 0 }],
            guard: None,
            line: 3,
        },
    ];

    static TABLE: Table = Table { source: "rules/test.rules", nodes: NODES, rules: RULES };

    fn add(terms: &mut Terms, second: usize) -> usize {
        let first = terms.app("v0", &[]);
        terms.app("add", &[first, second])
    }

    /// The concrete test is tried before the wildcard, so the rule about zero wins over the rule
    /// about any constant even though both of them match. That is the whole of what specificity
    /// order means here, and it falls out of the shape of the trie.
    #[test]
    fn the_rule_that_names_the_operand_beats_the_rule_that_takes_anything() {
        let mut terms = Terms::default();
        let zero = terms.constant(0);
        let term = add(&mut terms, zero);
        let found = TABLE.find(&terms, term).expect("a rule fires");
        assert_eq!(TABLE.rule(&found).pattern, "(add x 0)");
    }

    /// The bindings come back in the order the pattern binds them, which is the pre-order the
    /// replacement was flattened in, so a `Piece::Var` can be read as an index into them.
    #[test]
    fn a_match_gives_back_what_the_pattern_bound_in_the_order_it_bound_it() {
        let mut terms = Terms::default();
        let seven = terms.constant(7);
        let term = add(&mut terms, seven);
        let found = TABLE.find(&terms, term).expect("a rule fires");
        let rule = TABLE.rule(&found);
        assert_eq!(rule.pattern, "(add x k)");
        assert_eq!(rule.head(), Some("add_immediate"));
        assert_eq!(found.bindings.len(), 2);
        assert_eq!(found.bindings[1], seven);
        assert_eq!(terms.int(found.bindings[1]), Some(7));
    }

    /// A guard that does not hold is a rule that did not match, and there is nothing else to
    /// try, so the answer is nothing rather than the wrong rule.
    #[test]
    fn a_guard_that_refuses_takes_its_rule_out_of_the_running() {
        let mut terms = Terms::default();
        let negative = terms.constant(-1);
        let term = add(&mut terms, negative);
        assert_eq!(TABLE.find(&terms, term), None);
    }

    /// The same guard against an operand that is not a constant at all. A guard is a claim about
    /// a number, so a register makes it false rather than an error.
    #[test]
    fn a_guard_about_a_number_refuses_an_operand_that_is_not_one() {
        let mut terms = Terms::default();
        let other = terms.app("v1", &[]);
        let term = add(&mut terms, other);
        assert_eq!(TABLE.find(&terms, term), None);
    }

    #[test]
    fn a_term_no_rule_covers_finds_no_rule() {
        let mut terms = Terms::default();
        let x = terms.app("v0", &[]);
        let y = terms.app("v1", &[]);
        let term = terms.app("no.such.head", &[x, y]);
        assert_eq!(TABLE.find(&terms, term), None);
    }

    /// The rule that writes one name twice. Both operands are the same term, so the test that
    /// they are holds and the rule fires, and what comes back is the one binding the pattern
    /// made rather than two.
    #[test]
    fn a_pattern_that_names_one_hole_twice_matches_a_term_that_has_one_thing_in_both() {
        let mut terms = Terms::default();
        let x = terms.app("v0", &[]);
        let term = terms.app("and", &[x, x]);
        let found = TABLE.find(&terms, term).expect("a rule fires");
        assert_eq!(TABLE.rule(&found).pattern, "(and x x)");
        assert_eq!(found.bindings, vec![x]);
    }

    /// The same rule against two different terms. There is no wildcard beside the test, so a
    /// conjunction of two things is a conjunction no rule covers rather than one this rule
    /// wrongly claims.
    #[test]
    fn a_pattern_that_names_one_hole_twice_refuses_a_term_that_has_two_things_in_it() {
        let mut terms = Terms::default();
        let x = terms.app("v0", &[]);
        let y = terms.app("v1", &[]);
        let term = terms.app("and", &[x, y]);
        assert_eq!(TABLE.find(&terms, term), None);
    }

    /// A match is what a caller keeps, so it says what it is when a test prints it.
    #[test]
    fn a_match_names_the_rule_it_found() {
        let mut terms = Terms::default();
        let zero = terms.constant(0);
        let term = add(&mut terms, zero);
        assert_eq!(TABLE.find(&terms, term), Some(Match { rule: 0, bindings: vec![term - 1] }));
    }
}
