//! A map from a name to a value, kept in a stack of scopes.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.4.
//!
//! Two passes need this and they need the same thing from it. The parser holds one per C
//! namespace to answer whether an identifier in a specifier list is a type name, which is the
//! one real ambiguity in C's grammar. Semantic analysis holds one per namespace to resolve a
//! use of a name to the declaration it refers to. Neither of them knows anything about the
//! other's values, so what is shared is the scoping and not what is scoped, and it lives here
//! rather than being written twice and drifting.
//!
//! Nothing in here knows what C is. It is a name, a value, and the rule that an inner binding
//! hides an outer one until its scope closes.

use std::collections::HashMap;

use crate::intern::Symbol;

/// One binding, and the scope it was made in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Binding<V> {
    depth: u32,
    value: V,
}

/// A map from a name to a value, in a stack of scopes.
///
/// A lookup has to find the innermost binding of a name, and a scope closing has to expose
/// whatever that name meant outside it. Walking a stack of scopes would make every lookup cost
/// the depth, and a compiler looks up every identifier it reads, so the shape is inverted: one
/// map from name to the stack of bindings for that name, innermost last, plus a log of the
/// names bound in each open scope so that closing one knows what to undo.
#[derive(Debug)]
pub struct ScopeMap<V> {
    /// The bindings of each name, innermost last. An empty stack means the name is not bound,
    /// and the entry is kept rather than removed so that the allocation is reused by the next
    /// declaration of that name, which in a header is usually the same names again.
    bindings: HashMap<Symbol, Vec<Binding<V>>>,
    /// Every name bound in an open scope, in the order it was bound.
    log: Vec<Symbol>,
    /// Where each open scope starts in `log`. The file scope is not in here, which is what
    /// makes it impossible to close.
    marks: Vec<usize>,
}

impl<V> Default for ScopeMap<V> {
    fn default() -> Self {
        ScopeMap { bindings: HashMap::new(), log: Vec::new(), marks: Vec::new() }
    }
}

impl<V: Copy> ScopeMap<V> {
    /// An empty namespace, with the file scope open.
    #[must_use]
    pub fn new() -> Self {
        ScopeMap::default()
    }

    /// How many scopes are open. The file scope counts, so this is never zero.
    #[inline]
    #[must_use]
    pub fn depth(&self) -> u32 {
        // The count is bounded by the nesting the parser accepted, which is capped long before
        // this could overflow.
        self.marks.len() as u32 + 1
    }

    /// Whether the only open scope is the file scope.
    #[inline]
    #[must_use]
    pub fn at_file_scope(&self) -> bool {
        self.marks.is_empty()
    }

    /// Opens a scope.
    pub fn push(&mut self) {
        self.marks.push(self.log.len());
    }

    /// Closes the innermost scope, exposing whatever its names meant outside it.
    ///
    /// # Panics
    ///
    /// Panics on closing the file scope, which nothing in C does and which would leave the
    /// namespace unable to hold a declaration.
    pub fn pop(&mut self) {
        let mark = self.marks.pop().expect("the file scope is never closed");
        while self.log.len() > mark {
            let name = self.log.pop().expect("the log is longer than the mark");
            if let Some(stack) = self.bindings.get_mut(&name) {
                stack.pop();
            }
        }
    }

    /// Binds `name` in the innermost scope, and gives back what it was already bound to *in
    /// that same scope*.
    ///
    /// A returned value is a redeclaration, which is the caller's to judge: `int x; int x;` is
    /// fine at file scope and `typedef int T; T T;` is not, and neither decision belongs here.
    /// Shadowing an outer binding is not a redeclaration and gives back [`None`].
    pub fn declare(&mut self, name: Symbol, value: V) -> Option<V> {
        let depth = self.depth();
        let stack = self.bindings.entry(name).or_default();
        match stack.last_mut() {
            Some(top) if top.depth == depth => {
                let was = top.value;
                top.value = value;
                Some(was)
            }
            _ => {
                stack.push(Binding { depth, value });
                self.log.push(name);
                None
            }
        }
    }

    /// Binds `name` in the file scope from wherever the caller is, and answers whether it took.
    ///
    /// For the declaration a program did not write. A builtin used inside a function is
    /// declared where C says the implementation declared it, which is the file scope, so that
    /// what it means does not change when the block it was first used in closes.
    ///
    /// It takes only when the name is bound nowhere, which is the caller's own condition: this
    /// is reached because a lookup found nothing. A name bound anywhere is left alone rather
    /// than bound underneath, because the binding it already has may be the one being closed
    /// over and this has no log entry to undo.
    pub fn declare_at_file_scope(&mut self, name: Symbol, value: V) -> bool {
        let stack = self.bindings.entry(name).or_default();
        if !stack.is_empty() {
            return false;
        }
        // Not logged, which is what makes it survive every scope that closes over it. The log
        // is what a `pop` undoes, and the file scope is below the first mark and so is never
        // undone whether it is logged or not.
        stack.push(Binding { depth: 1, value });
        true
    }

    /// What `name` is bound to in the innermost scope that binds it.
    #[must_use]
    pub fn get(&self, name: Symbol) -> Option<V> {
        Some(self.bindings.get(&name)?.last()?.value)
    }

    /// What `name` is bound to in the innermost scope alone, ignoring the ones outside it.
    #[must_use]
    pub fn get_here(&self, name: Symbol) -> Option<V> {
        let depth = self.depth();
        let top = self.bindings.get(&name)?.last()?;
        (top.depth == depth).then_some(top.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A symbol the interner would have handed out. Nothing here reads a spelling.
    const X: Symbol = Symbol::from_raw(1);

    #[test]
    fn only_the_innermost_scope_counts_as_here() {
        let mut names = ScopeMap::new();
        names.declare(X, 1);
        names.push();
        assert_eq!(names.get(X), Some(1));
        assert_eq!(names.get_here(X), None);
        assert_eq!(names.depth(), 2);
        names.pop();
        assert_eq!(names.get_here(X), Some(1));
    }

    #[test]
    fn an_inner_binding_hides_an_outer_one_and_gives_it_back() {
        let mut names = ScopeMap::new();
        names.declare(X, 1);
        names.push();
        assert_eq!(names.declare(X, 2), None);
        assert_eq!(names.get(X), Some(2));
        names.pop();
        assert_eq!(names.get(X), Some(1));
    }

    #[test]
    fn a_second_binding_in_one_scope_is_a_redeclaration_and_says_what_it_was() {
        let mut names = ScopeMap::new();
        assert_eq!(names.declare(X, 1), None);
        assert_eq!(names.declare(X, 2), Some(1));
        assert_eq!(names.get(X), Some(2));
    }

    #[test]
    fn a_file_scope_binding_made_from_inside_outlives_the_block_it_was_made_in() {
        let mut names = ScopeMap::new();
        names.push();
        names.push();
        assert!(names.declare_at_file_scope(X, 1));
        assert_eq!(names.get(X), Some(1));
        names.pop();
        names.pop();
        assert!(names.at_file_scope());
        assert_eq!(names.get(X), Some(1), "the block it was used in is not where it was bound");
        assert_eq!(names.get_here(X), Some(1));
    }

    #[test]
    fn a_name_that_already_means_something_is_left_meaning_it() {
        let mut names = ScopeMap::new();
        names.push();
        names.declare(X, 1);
        assert!(!names.declare_at_file_scope(X, 2));
        assert_eq!(names.get(X), Some(1));
        names.pop();
        assert_eq!(names.get(X), None);
    }

    #[test]
    fn closing_a_scope_leaves_nothing_behind() {
        let mut names = ScopeMap::new();
        assert!(names.at_file_scope());
        for depth in 0..64 {
            names.push();
            names.declare(X, depth);
        }
        assert_eq!(names.depth(), 65);
        for _ in 0..64 {
            names.pop();
        }
        assert!(names.at_file_scope());
        assert_eq!(names.get(X), None);
    }

    #[test]
    #[should_panic(expected = "the file scope is never closed")]
    fn the_outermost_scope_cannot_be_closed() {
        ScopeMap::<u32>::new().pop();
    }
}
