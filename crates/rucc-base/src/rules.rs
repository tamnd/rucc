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
//! becomes a branch in [`Node::same`] rather than a hole, and it asks the subject whether the two
//! are the same thing rather than comparing nodes, because a node is a place and two places can
//! hold one value. It is a concrete test, so it is tried before the wildcard for the same reason
//! every other test is: a rule about one value in both operands is more specific than a rule
//! about any two.
//!
//! # Order
//!
//! At every node the concrete tests are tried before the branch that takes anything, so a rule
//! naming an operand is tried before a rule taking whatever is there. That is the maximal munch
//! `spec/10-backend.md` asks for, and it falls out of the shape of the trie rather than being
//! sorted for. Among rules that are equally specific the first one written wins.
//!
//! The concrete tests are three kinds of question and they are asked in this order: the head of
//! the term, then its value as a constant, then whether it is what an earlier binding took.
//! `spec/optimizer/36-lowering-and-isel.md` section 36.5 asks that the order be stated rather
//! than left to be read out of what the matcher does, so it is stated here, next to the walk that
//! applies it. It decides nothing in any rule set shipped today, because deciding something would
//! need one node to ask two kinds of question about one place and none does, which is a number
//! `rucc-rules` prints in the header of every table it generates.
//!
//! # Finding a branch
//!
//! A term has one head and a constant has one value, so at most one head branch and at most one
//! value branch can match, and the two lists are sorted by the thing they are asked about. That
//! makes finding the branch a binary search rather than a walk over the node, which is the
//! difference section 36.5 is about: the widest node of the x86-64 rule set has a hundred and
//! sixty seven heads on it, and the selector reaches that node once for every instruction in the
//! program. A repeat of an earlier binding is not searchable, because two of them can hold the
//! same value, so those stay in the order the rules were written and there are never many.
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

/// One node of the trie over the patterns.
///
/// The branches are held by the kind of question they ask rather than in one list, which is what
/// lets the two that can be searched be searched.
#[derive(Debug, Clone, Copy)]
pub struct Node {
    /// The branches taken on the head of the subterm, as the name, how many arguments it takes,
    /// and where to go. Sorted by the first two, which is what [`Node::branch`] needs.
    pub heads: &'static [(&'static str, usize, u32)],
    /// The branches taken on the value of a subterm that is a constant, sorted by the value.
    pub ints: &'static [(i128, u32)],
    /// The branches taken when the subterm is the same thing as a binding this pattern already
    /// made, named by which binding it is. A pattern writes one where it writes a name for the
    /// second time, so this is how `x & x` is told apart from `x & y`. In the order the rules
    /// were written, because two of them can match one subterm.
    pub same: &'static [(usize, u32)],
    /// The branch that takes anything, and the name the first rule to reach it gave that hole.
    pub wildcard: Option<(&'static str, u32)>,
    /// The rule that ends here, if one does.
    pub accept: Option<u32>,
}

impl Node {
    /// The branch for a term with this head and this many arguments, if the node has one.
    ///
    /// A binary search, which is the whole point of the list being sorted. At most one branch can
    /// answer, so nothing about which rule fires depends on the list being in this order rather
    /// than in the order the rules were written.
    #[must_use]
    pub fn branch(&self, head: &str, arity: usize) -> Option<u32> {
        let found = self
            .heads
            .binary_search_by(|(have, count, _)| have.cmp(&head).then(count.cmp(&arity)))
            .ok()?;
        Some(self.heads[found].2)
    }

    /// The branch for a constant of this value, if the node has one.
    #[must_use]
    pub fn literal(&self, value: i128) -> Option<u32> {
        let found = self.ints.binary_search_by(|(have, _)| have.cmp(&value)).ok()?;
        Some(self.ints[found].1)
    }
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
    /// A constant the rule works out from the ones the pattern matched.
    ///
    /// This is what lets a rule be written once per width rather than once per constant. A shift
    /// that stands in for a multiplication by a power of two shifts by the log of that power, and
    /// the log is a number no rule can write down until it has seen which power it matched.
    Computed {
        /// The computation as the rule file writes it, for anything that has to say what it did.
        text: &'static str,
        /// What it works out.
        work: Computation,
    },
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

/// A number worked out from the constants a pattern matched.
///
/// Handed one entry per binding, the same as a [`Guard`] is, and for the same reason: the
/// computation is written in the names the pattern bound and those are positions by the time it
/// runs. It gives nothing back when a binding it reads is not a constant, which is the answer a
/// guard gives as false, and the rule does not fire.
pub type Computation = fn(&[Option<i128>]) -> Option<i128>;

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

        // The head of the term, which is the question nearly every branch of nearly every node
        // is about and the one that has to be found rather than looked for.
        if let Some(next) = head.and_then(|(name, arity)| node.branch(name, arity)) {
            if let Some(rule) = self.take(subject, next, (term, head), &left, bindings) {
                return Some(rule);
            }
        }

        // Its value, if it is a constant and if this node asks about one. The emptiness is
        // checked first because asking the subject for a value costs something and most nodes
        // have nothing to compare it against.
        if !node.ints.is_empty() {
            if let Some(next) = subject.int(term).and_then(|value| node.literal(value)) {
                if let Some(rule) = self.take(subject, next, (term, head), &left, bindings) {
                    return Some(rule);
                }
            }
        }

        // A repeat of an earlier binding. The binding is always there, because a pattern only
        // writes a name for the second time after it has written it once and the trie keeps that
        // order.
        for &(index, next) in node.same {
            if bindings.get(index).is_some_and(|&bound| subject.same(bound, term)) {
                if let Some(rule) = self.take(subject, next, (term, head), &left, bindings) {
                    return Some(rule);
                }
            }
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

    /// Follow one branch, and give the bindings back as they were if it led nowhere.
    ///
    /// What goes on the stack is the arguments of the term, innermost last, whenever the term has
    /// any. That is the same for every kind of branch, because what a branch decided is that this
    /// subterm is matched and the walk carries on into what is under it.
    fn take<S: Subject>(
        &self,
        subject: &S,
        next: u32,
        term: (S::Node, Option<(&str, usize)>),
        left: &[S::Node],
        bindings: &mut Vec<S::Node>,
    ) -> Option<usize> {
        let (term, head) = term;
        let mut deeper = left.to_vec();
        if let Some((_, arity)) = head {
            for index in (0..arity).rev() {
                deeper.push(subject.arg(term, index));
            }
        }
        let depth = bindings.len();
        if let Some(rule) = self.run(subject, next as usize, deeper, bindings) {
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
    use super::{Match, Node, Piece, Rule, Subject, Table};

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
    /// A node with nothing on it, so that the ones below say only what they are about.
    const NOTHING: Node = Node { heads: &[], ints: &[], same: &[], wildcard: None, accept: None };

    static NODES: &[Node] = &[
        // 0, the root.
        Node { heads: &[("add", 2, 1), ("and", 2, 5)], ..NOTHING },
        // 1, the first operand.
        Node { wildcard: Some(("x", 2)), ..NOTHING },
        // 2, the second operand.
        Node { ints: &[(0, 3)], wildcard: Some(("k", 4)), ..NOTHING },
        // 3, an addition of zero.
        Node { accept: Some(0), ..NOTHING },
        // 4, an addition of anything, if the guard holds.
        Node { accept: Some(1), ..NOTHING },
        // 5, the first operand of the conjunction, which is the one that binds.
        Node { wildcard: Some(("x", 6)), ..NOTHING },
        // 6, the second operand, which has to be what the first one bound.
        Node { same: &[(0, 7)], ..NOTHING },
        // 7, a conjunction of one thing with itself.
        Node { accept: Some(2), ..NOTHING },
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

    /// The branch is found rather than looked for, which is the thing a node being sorted buys.
    /// A node as wide as the root of a real rule set answers in the same number of comparisons a
    /// node with eight branches does, and it answers about the head it was never given by not
    /// finding one rather than by reading to the end.
    #[test]
    fn a_branch_is_found_by_searching_the_node_and_not_by_reading_it() {
        static WIDE: &[(&str, usize, u32)] = &[
            ("add.i16", 2, 1),
            ("add.i32", 2, 2),
            ("add.i64", 2, 3),
            ("add.i64", 3, 4),
            ("sub.i32", 2, 5),
            ("sub.i64", 2, 6),
            ("xor.i8", 2, 7),
        ];
        let node = Node { heads: WIDE, ..NOTHING };
        assert!(WIDE.is_sorted(), "the search is only a search if the node is in order");
        assert_eq!(node.branch("add.i64", 2), Some(3));
        assert_eq!(node.branch("add.i16", 2), Some(1));
        assert_eq!(node.branch("xor.i8", 2), Some(7));
        // The same name at two arities is two branches, and they are told apart.
        assert_eq!(node.branch("add.i64", 3), Some(4));
        // A head no branch is about, and one the node has at another arity, are both nothing.
        assert_eq!(node.branch("mul.i64", 2), None);
        assert_eq!(node.branch("sub.i32", 3), None);
    }

    /// The same for a constant, which is the other kind of branch that can be searched.
    #[test]
    fn a_literal_is_found_by_searching_too() {
        let node = Node { ints: &[(-8, 1), (0, 2), (1, 3), (4096, 4)], ..NOTHING };
        assert_eq!(node.literal(-8), Some(1));
        assert_eq!(node.literal(0), Some(2));
        assert_eq!(node.literal(4096), Some(4));
        assert_eq!(node.literal(7), None);
    }

    /// The order the kinds of question are asked in, which is the heuristic the module doc
    /// states. It only decides anything when one node asks two kinds about one place and the
    /// subject answers both, which is why this needs a subject of its own: the one above answers
    /// either what a term is called or what number it is, never both, and so does the IR. What is
    /// asserted is the order that is written down, so that a rule set which starts to depend on
    /// it gets the answer somebody chose rather than the one that fell out.
    #[test]
    fn the_head_is_asked_about_before_the_value_and_the_value_before_a_repeat() {
        /// `(f a a)`, where each operand is an application and a number at the same time and the
        /// two of them are one thing. Every question a node can ask is true of them, so which
        /// one is asked first is the only thing that decides the answer.
        #[derive(Debug)]
        struct Both;

        impl Subject for Both {
            type Node = u8;

            fn head(&self, node: u8) -> Option<(&str, usize)> {
                if node == 0 { Some(("f", 2)) } else { Some(("k", 0)) }
            }

            fn int(&self, node: u8) -> Option<i128> {
                if node == 0 { None } else { Some(7) }
            }

            fn arg(&self, _: u8, _: usize) -> u8 {
                1
            }

            fn same(&self, _: u8, _: u8) -> bool {
                true
            }
        }

        static FOUR: &[Rule] = &[
            Rule { pattern: "the head", replacement: &[], guard: None, line: 1 },
            Rule { pattern: "the value", replacement: &[], guard: None, line: 2 },
            Rule { pattern: "the repeat", replacement: &[], guard: None, line: 3 },
            Rule { pattern: "the hole", replacement: &[], guard: None, line: 4 },
        ];

        /// The four ends, and in front of them the node that binds the first operand so that
        /// there is something for a repeat to be a repeat of.
        fn table(second: &'static Node) -> Table {
            let nodes: &'static [Node] = Box::leak(Box::new([
                Node { heads: &[("f", 2, 1)], ..NOTHING },
                Node { wildcard: Some(("x", 2)), ..NOTHING },
                *second,
                Node { accept: Some(0), ..NOTHING },
                Node { accept: Some(1), ..NOTHING },
                Node { accept: Some(2), ..NOTHING },
                Node { accept: Some(3), ..NOTHING },
            ]));
            Table { source: "rules/test.rules", nodes, rules: FOUR }
        }

        // All three kinds on one node, with a hole behind them.
        static MIXED: Node = Node {
            heads: &[("k", 0, 3)],
            ints: &[(7, 4)],
            same: &[(0, 5)],
            wildcard: Some(("y", 6)),
            accept: None,
        };
        assert_eq!(table(&MIXED).find(&Both, 0).map(|found| found.rule), Some(0));

        // The same node without the head, which is what puts the value in front.
        static WITHOUT_HEAD: Node = Node { heads: &[], ..MIXED };
        assert_eq!(table(&WITHOUT_HEAD).find(&Both, 0).map(|found| found.rule), Some(1));

        // And without either, which leaves the repeat in front of the hole. That last pair is
        // the one that is not a heuristic: a concrete question always comes before the hole.
        static REPEAT: Node = Node { ints: &[], ..WITHOUT_HEAD };
        assert_eq!(table(&REPEAT).find(&Both, 0).map(|found| found.rule), Some(2));

        // And with nothing concrete left, the hole.
        static HOLE: Node = Node { same: &[], ..REPEAT };
        assert_eq!(table(&HOLE).find(&Both, 0).map(|found| found.rule), Some(3));
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
