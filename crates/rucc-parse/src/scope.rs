//! Scopes, and the typedef decision that rests on them.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.4.
//!
//! C's grammar is ambiguous without knowing which identifiers are type names, because `(A)*B`
//! is a cast when `A` is a type and a multiplication when it is not. The parser resolves that
//! here, against scopes it maintains itself, and there is no feedback channel to the lexer.
//! Feeding the answer back to the lexer is the traditional approach and it makes the lexer's
//! state depend on how far the parser has got, which is what makes lookahead and error recovery
//! painful in the compilers that do it.
//!
//! # The hazards
//!
//! Each of these is a real bug in a real compiler, and each has a test below.
//!
//! A declarator introduces its name at the *end* of the declarator, not at the start, so
//! `typedef int T; void f(int T, T x);` has `T` as a parameter name and `T x` is then an error,
//! while `typedef int T; T T;` reads the specifier `T` as the type and then declares a variable
//! of that name.
//!
//! Tags occupy a namespace of their own, so `struct S` does not disturb what a bare `S` means,
//! and a typedef name shadowed by an inner declaration comes back when that scope closes.
//!
//! The scoping itself is [`ScopeMap`], in `rucc-base`, because semantic
//! analysis needs the same structure with different values in it.
//!
//! # What is not here
//!
//! Two of C's four namespaces. Labels are function wide rather than block scoped and nothing
//! about them is ambiguous, so the function parser collects them and this stack would only be
//! in the way. Members belong to the record that declares them and are reached through a type
//! rather than through a scope, which makes them semantic analysis's problem and not a parsing
//! decision at all.

use rucc_base::{ScopeMap, Symbol};

/// What an identifier means where the parser is looking at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentKind {
    /// Declared with `typedef`, so a use of it in a specifier list is a type name.
    Typedef,
    /// Declared as anything else: an object, a function, a parameter, an enumerator.
    Ordinary,
}

/// Which keyword introduced a tag.
///
/// Kept because the three do not interchange, and because the diagnostic for using the wrong
/// one has to name what was declared. Whether a mismatch is an error is semantic analysis's
/// call; the parser only records what it saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagKind {
    /// `struct`.
    Struct,
    /// `union`.
    Union,
    /// `enum`.
    Enum,
}

/// The scopes the parser keeps, across the namespaces it has to hold apart.
#[derive(Debug, Default)]
pub struct Scopes {
    ordinary: ScopeMap<IdentKind>,
    tags: ScopeMap<TagKind>,
}

impl Scopes {
    /// Empty scopes, with the file scope open.
    #[must_use]
    pub fn new() -> Self {
        Scopes::default()
    }

    /// Opens a scope in every namespace.
    ///
    /// Both are pushed together because C opens them together. A parameter list is a scope of
    /// its own, which is why `void f(struct S *p);` declares a tag that is gone by the time the
    /// next declaration is read, and getting that wrong in one namespace and not the other is
    /// how the two drift out of step.
    pub fn push(&mut self) {
        self.ordinary.push();
        self.tags.push();
    }

    /// Closes the innermost scope in every namespace.
    ///
    /// # Panics
    ///
    /// Panics on closing the file scope.
    pub fn pop(&mut self) {
        self.ordinary.pop();
        self.tags.pop();
    }

    /// Whether the only open scope is the file scope.
    #[inline]
    #[must_use]
    pub fn at_file_scope(&self) -> bool {
        self.ordinary.at_file_scope()
    }

    /// How many scopes are open, the file scope counting as one.
    #[inline]
    #[must_use]
    pub fn depth(&self) -> u32 {
        self.ordinary.depth()
    }

    /// What `name` means here, and [`None`] when it has not been declared.
    #[inline]
    #[must_use]
    pub fn ident(&self, name: Symbol) -> Option<IdentKind> {
        self.ordinary.get(name)
    }

    /// Whether `name` in a specifier list is a type name.
    ///
    /// This is the answer the whole ambiguity turns on. An identifier that has not been
    /// declared at all is not a type name: the declaration it is missing is an error, and
    /// guessing that an unknown name is a type in the hope of a better parse produces a cascade
    /// out of one typo.
    #[inline]
    #[must_use]
    pub fn is_typedef_name(&self, name: Symbol) -> bool {
        self.ordinary.get(name) == Some(IdentKind::Typedef)
    }

    /// Declares `name` in the innermost scope, and gives back what it was in that same scope.
    ///
    /// Called at the end of a declarator rather than at its start, which is what makes
    /// `typedef int T; T T;` read the way C says it does.
    pub fn declare(&mut self, name: Symbol, kind: IdentKind) -> Option<IdentKind> {
        self.ordinary.declare(name, kind)
    }

    /// Declares a tag, and gives back what it was in the same scope.
    pub fn declare_tag(&mut self, name: Symbol, kind: TagKind) -> Option<TagKind> {
        self.tags.declare(name, kind)
    }

    /// What tag `name` names here.
    #[inline]
    #[must_use]
    pub fn tag(&self, name: Symbol) -> Option<TagKind> {
        self.tags.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Symbols the interner would have handed out. Nothing here reads a spelling, so the
    /// numbers stand in for one.
    const T: Symbol = Symbol::from_raw(1);
    const X: Symbol = Symbol::from_raw(2);

    #[test]
    fn an_undeclared_name_is_not_a_type_name() {
        let scopes = Scopes::new();
        assert_eq!(scopes.ident(T), None);
        assert!(!scopes.is_typedef_name(T));
    }

    #[test]
    fn a_parameter_takes_the_name_away_from_the_typedef() {
        // typedef int T; void f(int T, T x);
        let mut scopes = Scopes::new();
        scopes.declare(T, IdentKind::Typedef);
        scopes.push();
        assert!(scopes.is_typedef_name(T));
        // The first parameter's declarator ends, and `T` is its name.
        scopes.declare(T, IdentKind::Ordinary);
        // Which is why the second parameter's `T` is no longer a type and `T x` is an error.
        assert!(!scopes.is_typedef_name(T));
        assert_eq!(scopes.ident(T), Some(IdentKind::Ordinary));
        scopes.pop();
        assert!(scopes.is_typedef_name(T));
    }

    #[test]
    fn a_variable_may_take_the_name_of_the_typedef_that_gave_it_its_type() {
        // typedef int T; T T;
        let mut scopes = Scopes::new();
        scopes.declare(T, IdentKind::Typedef);
        // The specifier list is read first, while `T` is still a type name.
        assert!(scopes.is_typedef_name(T));
        // Then the declarator ends and the name is bound over the top of it, in the same scope,
        // which is the redeclaration the caller reports.
        assert_eq!(scopes.declare(T, IdentKind::Ordinary), Some(IdentKind::Typedef));
        assert_eq!(scopes.ident(T), Some(IdentKind::Ordinary));
    }

    #[test]
    fn a_typedef_is_exposed_again_when_the_inner_scope_closes() {
        let mut scopes = Scopes::new();
        scopes.declare(T, IdentKind::Typedef);
        scopes.push();
        assert_eq!(scopes.declare(T, IdentKind::Ordinary), None);
        assert_eq!(scopes.ident(T), Some(IdentKind::Ordinary));
        scopes.push();
        assert_eq!(scopes.declare(T, IdentKind::Typedef), None);
        assert!(scopes.is_typedef_name(T));
        scopes.pop();
        assert_eq!(scopes.ident(T), Some(IdentKind::Ordinary));
        scopes.pop();
        assert!(scopes.is_typedef_name(T));
    }

    #[test]
    fn a_tag_does_not_disturb_the_ordinary_name() {
        // typedef int T; struct T { int x; }; T v;
        let mut scopes = Scopes::new();
        scopes.declare(T, IdentKind::Typedef);
        assert_eq!(scopes.declare_tag(T, TagKind::Struct), None);
        assert_eq!(scopes.tag(T), Some(TagKind::Struct));
        assert!(scopes.is_typedef_name(T));
        assert_eq!(scopes.tag(X), None);
    }

    #[test]
    fn a_binding_in_an_inner_scope_is_not_a_redeclaration() {
        let mut scopes = Scopes::new();
        assert_eq!(scopes.declare(X, IdentKind::Ordinary), None);
        scopes.push();
        assert_eq!(scopes.declare(X, IdentKind::Ordinary), None);
        assert_eq!(scopes.declare(X, IdentKind::Typedef), Some(IdentKind::Ordinary));
        scopes.pop();
    }

    #[test]
    fn closing_a_scope_leaves_nothing_behind() {
        let mut scopes = Scopes::new();
        assert!(scopes.at_file_scope());
        for _ in 0..64 {
            scopes.push();
            scopes.declare(X, IdentKind::Ordinary);
            scopes.declare_tag(X, TagKind::Union);
        }
        assert_eq!(scopes.depth(), 65);
        for _ in 0..64 {
            scopes.pop();
        }
        assert!(scopes.at_file_scope());
        assert_eq!(scopes.ident(X), None);
        assert_eq!(scopes.tag(X), None);
    }

    #[test]
    #[should_panic(expected = "the file scope is never closed")]
    fn the_file_scope_cannot_be_closed() {
        Scopes::new().pop();
    }
}
