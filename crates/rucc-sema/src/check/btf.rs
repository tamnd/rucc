//! `btf_decl_tag` and `btf_type_tag`, gcc 16's way of tagging a declaration or a type with a
//! string for BPF's type format to carry, which is what the kernel's `__user`, `__rcu` and
//! `__percpu` are when it is built for BTF.
//!
//! gcc writes the tags into the debugging information, and the program it compiles is the same
//! with them as without. The strings of the `btf_decl_tag`s on a function, an object, a
//! parameter or a member are kept beside the tree for the debugging information, which writes
//! them as gcc does. A `btf_type_tag` is not written, since a type here has no variant for each
//! tag it was written with. What is left is what gcc checks of them, in its words:
//!
//! * Each takes exactly one argument, and is refused with gcc's count otherwise.
//! * That argument is a string literal, of `char` or of `char8_t`. Anything else is refused, and
//!   a wide string is refused as one.
//! * `btf_decl_tag` is about a declaration, so on a structure, a union or an enumeration it is
//!   ignored with gcc's warning that it does not apply to types.
//! * `btf_type_tag` is about the type a declaration has, and a function's type is not one it
//!   tags, so on a function it is ignored with gcc's warning that it does not apply to functions.
//!
//! The arguments are checked once for every tag in the unit, before anything else reads it, so
//! a tag the checker has no other reason to visit, on a cast or a parameter, is checked all the
//! same. gcc checks the place first, and says only that a `btf_decl_tag` on a type does not
//! apply there whatever its argument is. Here a tag whose argument is wrong is refused for that,
//! wherever it is, and only one whose argument is right is held against its place.
//!
//! gcc reads a lone name as an expression and says first that nothing declares it, where
//! nothing does. Here a name is not looked up, and is refused only for not being a string.

use rucc_ast::{AttrArg, AttrList, AttrSyntax, Attribute};
use rucc_diag::{Diagnostic, Span};
use rucc_lex::Encoding;
use rucc_types::{TypeId, TypeKind};

use crate::check::Checker;

/// The code of the errors about a tag's argument.
const BTF_ARGUMENT: &str = "E0858";

/// What is wrong with the arguments of one tag.
enum Fault {
    /// Not one argument but this many.
    Count(usize),
    /// An argument that is not a string literal.
    NotString,
    /// A string literal whose elements are wider than a `char`.
    Wide,
}

impl Checker<'_> {
    /// Refuses every tag in the unit whose arguments gcc refuses, in the order they were written.
    pub(in crate::check) fn check_btf_tags(&mut self) {
        let ast = self.ast;
        // The same attribute can be in the tree's list twice, where the parser read it once and
        // then again, and it is said once.
        let mut said: Vec<Span> = Vec::new();
        for attr in ast.attributes() {
            let Some(name) = self.btf_tag(attr) else {
                continue;
            };
            let Some(fault) = self.btf_fault(attr) else {
                continue;
            };
            if said.contains(&attr.span) {
                continue;
            }
            said.push(attr.span);
            let refused = match fault {
                Fault::Count(count) => {
                    let what =
                        format!("wrong number of arguments specified for '{name}' attribute");
                    let note = format!("expected 1, found {count}");
                    Diagnostic::error(what, attr.span).note(note, attr.span)
                }
                Fault::NotString => {
                    let what = format!("'{name}' attribute requires a string argument");
                    Diagnostic::error(what, attr.span)
                }
                Fault::Wide => {
                    let what =
                        format!("unsupported wide string type argument in '{name}' attribute");
                    Diagnostic::error(what, attr.span)
                }
            };
            self.report(refused.with_code(BTF_ARGUMENT));
        }
    }

    /// Warns of each `btf_decl_tag` written on the structure, union or enumeration being
    /// defined.
    pub(in crate::check) fn btf_decl_tag_on_type(&mut self, attrs: AttrList) {
        self.btf_misplaced(&[attrs], "btf_decl_tag", "types");
    }

    /// Warns of each `btf_type_tag` on a declaration of that type, where that type is a
    /// function's.
    pub(in crate::check) fn btf_type_tag_on_function(&mut self, lists: &[AttrList], ty: TypeId) {
        if matches!(self.types.kind(self.types.canonical(ty)), TypeKind::Function(_)) {
            self.btf_misplaced(lists, "btf_type_tag", "functions");
        }
    }

    /// The strings of the `btf_decl_tag`s in the lists whose arguments are right, in the order
    /// gcc keeps them on the declaration: the lists are the attributes after the declarator and
    /// then the specifiers', gcc puts each attribute it reads on the front, and a tag the
    /// declaration already has is not put there again.
    pub(in crate::check) fn btf_decl_tags(&self, lists: &[AttrList]) -> Vec<Vec<u8>> {
        let ast = self.ast;
        let mut tags: Vec<Vec<u8>> = Vec::new();
        for &attrs in lists {
            for attr in &ast[attrs] {
                if self.btf_tag(attr) != Some("btf_decl_tag") || self.btf_fault(attr).is_some() {
                    continue;
                }
                let [AttrArg::Expr(expr)] = ast[attr.args][..] else { continue };
                let rucc_ast::Expr::Str(string) = ast[expr] else { continue };
                // The string up to its first zero, which is all of it a reader of the debugging
                // information sees.
                let tag: Vec<u8> = ast[string]
                    .elements
                    .iter()
                    .take_while(|&&element| element != 0)
                    .map(|&element| element as u8)
                    .collect();
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
        }
        tags.reverse();
        tags
    }

    /// gcc's warning that the tag of that name does not apply to what it was written on, for
    /// each of them in the lists whose arguments are right.
    fn btf_misplaced(&mut self, lists: &[AttrList], name: &str, place: &str) {
        let ast = self.ast;
        for &attrs in lists {
            for attr in &ast[attrs] {
                if self.btf_tag(attr) == Some(name) && self.btf_fault(attr).is_none() {
                    let what = format!("'{name}' attribute does not apply to {place}");
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                }
            }
        }
    }

    /// Which of the two tags that is, where it is gcc's and the persona's gcc knows it.
    fn btf_tag(&self, attr: &Attribute) -> Option<&'static str> {
        if attr.namespace.is_none() && attr.syntax == AttrSyntax::Standard {
            return None;
        }
        Some(self.gnu_name(attr)).filter(|name| matches!(*name, "btf_decl_tag" | "btf_type_tag"))
    }

    /// What gcc refuses in the arguments of a tag, if anything.
    fn btf_fault(&self, attr: &Attribute) -> Option<Fault> {
        let args = &self.ast[attr.args];
        let [arg] = args[..] else {
            return Some(Fault::Count(args.len()));
        };
        let AttrArg::Expr(expr) = arg else {
            return Some(Fault::NotString);
        };
        let rucc_ast::Expr::Str(string) = self.ast[expr] else {
            return Some(Fault::NotString);
        };
        match self.ast[string].encoding {
            Encoding::Plain | Encoding::Utf8 => None,
            Encoding::Wide | Encoding::Utf16 | Encoding::Utf32 => Some(Fault::Wide),
        }
    }
}
