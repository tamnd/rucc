//! The scopes semantic analysis keeps, and what a name means in each of them.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.4.
//!
//! The scoping itself is [`ScopeMap`], in `rucc-base`, because the parser keeps the same
//! structure with different values in it. What is here is the values: C's namespaces, and what
//! a name in each of them resolves to once the declaration it refers to has been checked.
//!
//! Two of C's four namespaces are here. Labels are function wide rather than block scoped, so
//! the function checker holds them in a flat map and this stack would only be in the way.
//! Members belong to the record that declares them and are reached through a type rather than
//! through a scope, so they are a question for the type table.
//!
//! # Why the parser's answer is not enough
//!
//! The parser already resolved names, in the sense that it decided which of them were type
//! names. That is a different question and a smaller one: it needed to know whether `A` in
//! `(A)*b` was a type, and it never needed to know which `A`. This has to know which
//! declaration a use refers to, because the answer is what the use gets its type from and what
//! the object file eventually refers to.

use rucc_base::{ScopeMap, Symbol};
use rucc_types::TypeId;

use crate::decl::DeclId;

/// What an ordinary identifier names.
///
/// The four things C's ordinary namespace holds, which is objects, functions, typedef names and
/// enumerators. The first two are the same case here because a use of either is a use of a
/// declaration, and what separates them is the type it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    /// An object or a function, which is a declaration in the typed tree.
    Decl(DeclId),
    /// A `typedef` name, which is a name for a type and never appears in the tree.
    Typedef(TypeId),
    /// An enumerator, which is a constant and is folded into the expression that used it.
    Enumerator {
        /// The value, in the enumeration's underlying type.
        value: i128,
        /// The type the constant has, which is the enumeration in C23 and `int` before it.
        ty: TypeId,
    },
}

/// Which keyword introduced a tag.
///
/// A mismatch is an error and the diagnostic has to name what was declared, so the three are
/// kept apart rather than collapsed into the type they name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagKind {
    /// `struct`.
    Struct,
    /// `union`.
    Union,
    /// `enum`.
    Enum,
}

impl TagKind {
    /// How the keyword is spelled in a diagnostic.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            TagKind::Struct => "struct",
            TagKind::Union => "union",
            TagKind::Enum => "enum",
        }
    }
}

/// A tag, and the type it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tag {
    /// Which keyword declared it.
    pub kind: TagKind,
    /// The type, which exists from the point the tag is first mentioned and is incomplete
    /// until the definition is read.
    pub ty: TypeId,
}

/// The scopes of one translation unit.
#[derive(Debug, Default)]
pub struct Scopes {
    ordinary: ScopeMap<Binding>,
    tags: ScopeMap<Tag>,
}

impl Scopes {
    /// Empty scopes, with the file scope open.
    #[must_use]
    pub fn new() -> Scopes {
        Scopes::default()
    }

    /// Opens a scope in every namespace.
    ///
    /// Both are pushed together because C opens them together. A parameter list is a scope of
    /// its own, which is why the tag in `void f(struct S *p);` is gone by the next declaration,
    /// and getting that wrong in one namespace and not the other is how the two drift.
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
    #[must_use]
    pub fn at_file_scope(&self) -> bool {
        self.ordinary.at_file_scope()
    }

    /// How many scopes are open, the file scope counting as one.
    #[must_use]
    pub fn depth(&self) -> u32 {
        self.ordinary.depth()
    }

    /// Binds an ordinary identifier, and gives back what it was bound to in the same scope.
    ///
    /// A returned value is a redeclaration, which is the caller's to judge, since `int x; int
    /// x;` is one object at file scope and an error inside a function.
    pub fn declare(&mut self, name: Symbol, binding: Binding) -> Option<Binding> {
        self.ordinary.declare(name, binding)
    }

    /// Binds an ordinary identifier in the file scope from wherever the checking is.
    ///
    /// For a builtin, which C says the implementation declared and which therefore was not
    /// declared in whichever block first called it. Answers whether it took, which it does
    /// only when nothing else binds the name.
    pub fn declare_at_file_scope(&mut self, name: Symbol, binding: Binding) -> bool {
        self.ordinary.declare_at_file_scope(name, binding)
    }

    /// What an ordinary identifier names here.
    #[must_use]
    pub fn lookup(&self, name: Symbol) -> Option<Binding> {
        self.ordinary.get(name)
    }

    /// What an ordinary identifier names in the innermost scope that binds it to a binding
    /// `wanted` takes, looking outwards.
    #[must_use]
    pub fn lookup_where(&self, name: Symbol, wanted: impl Fn(Binding) -> bool) -> Option<Binding> {
        self.ordinary.get_where(name, wanted)
    }

    /// What an ordinary identifier names in the innermost scope alone.
    #[must_use]
    pub fn lookup_here(&self, name: Symbol) -> Option<Binding> {
        self.ordinary.get_here(name)
    }

    /// Binds a tag, and gives back what it was bound to in the same scope.
    pub fn declare_tag(&mut self, name: Symbol, tag: Tag) -> Option<Tag> {
        self.tags.declare(name, tag)
    }

    /// What tag a name names here.
    #[must_use]
    pub fn tag(&self, name: Symbol) -> Option<Tag> {
        self.tags.get(name)
    }

    /// What tag a name names in the innermost scope alone.
    ///
    /// This is the question `struct S;` asks, since a bare declaration of a tag declares a new
    /// type in this scope even where an outer one is visible, and `struct S *p;` asks the other
    /// one, since it refers to whatever `S` already means.
    #[must_use]
    pub fn tag_here(&self, name: Symbol) -> Option<Tag> {
        self.tags.get_here(name)
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Idx;
    use rucc_types::{IntKind, Types};

    use super::*;

    const S: Symbol = Symbol::from_raw(1);

    #[test]
    fn a_tag_and_an_ordinary_name_do_not_disturb_each_other() {
        let types = Types::new();
        let int = types.int(IntKind::Int);
        let mut scopes = Scopes::new();

        scopes.declare(S, Binding::Typedef(int));
        scopes.declare_tag(S, Tag { kind: TagKind::Struct, ty: int });

        assert_eq!(scopes.lookup(S), Some(Binding::Typedef(int)));
        assert_eq!(scopes.tag(S).map(|tag| tag.kind), Some(TagKind::Struct));
    }

    #[test]
    fn an_inner_declaration_hides_an_outer_one_until_its_scope_closes() {
        let outer = Binding::Decl(Idx::from_usize(0));
        let inner = Binding::Decl(Idx::from_usize(1));
        let mut scopes = Scopes::new();

        scopes.declare(S, outer);
        scopes.push();
        assert_eq!(scopes.declare(S, inner), None);
        assert_eq!(scopes.lookup(S), Some(inner));
        // Which is what makes a use resolve to a declaration rather than to a name.
        scopes.pop();
        assert_eq!(scopes.lookup(S), Some(outer));
    }

    #[test]
    fn a_tag_declared_again_in_an_inner_scope_is_a_new_type() {
        let types = Types::new();
        let int = types.int(IntKind::Int);
        let long = types.int(IntKind::Long);
        let mut scopes = Scopes::new();

        scopes.declare_tag(S, Tag { kind: TagKind::Struct, ty: int });
        scopes.push();
        // `struct S;` asks what is bound here and finds nothing, so it declares a new type.
        assert_eq!(scopes.tag_here(S), None);
        scopes.declare_tag(S, Tag { kind: TagKind::Struct, ty: long });
        assert_eq!(scopes.tag(S).map(|tag| tag.ty), Some(long));
        scopes.pop();
        assert_eq!(scopes.tag(S).map(|tag| tag.ty), Some(int));
    }

    #[test]
    fn a_redeclaration_in_one_scope_says_what_it_was() {
        let first = Binding::Decl(Idx::from_usize(0));
        let second = Binding::Decl(Idx::from_usize(1));
        let mut scopes = Scopes::new();

        assert_eq!(scopes.declare(S, first), None);
        assert_eq!(scopes.declare(S, second), Some(first));
    }
}
