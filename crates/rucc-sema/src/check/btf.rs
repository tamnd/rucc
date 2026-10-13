//! `btf_decl_tag` and `btf_type_tag`, gcc 16's way of tagging a declaration or a type with a
//! string for BPF's type format to carry, which is what the kernel's `__user`, `__rcu` and
//! `__percpu` are when it is built for BTF.
//!
//! gcc writes the tags into the debugging information, and the program it compiles is the same
//! with them as without. The strings of the `btf_decl_tag`s on a function, an object, a
//! parameter or a member are kept beside the tree for the debugging information, which writes
//! them as gcc does.
//!
//! A `btf_type_tag` tags one level of the type a declaration gives what it declares, and a type
//! here has no variant for each tag it was written with, so which level each tag is on is kept
//! beside the tree too, and the debugging information makes the tagged variants. The level is
//! where gcc puts it:
//!
//! * In the specifiers, or after the declarator, it is on the type the declaration gives what it
//!   declares, so `int __user *p` tags the pointer and `int __user a[3]` the array.
//! * After a `*`, it is on that pointer, so `int * __user * pp` tags what `pp` points at.
//! * A `[[...]]` written after a specifier is C23's, and is on the type the specifiers name.
//! * Written after the body of a structure, a union or an enumeration, it tags that type itself.
//!
//! What is left is what gcc checks of them, in its words:
//!
//! * Each takes exactly one argument, and is refused with gcc's count otherwise.
//! * That argument is a string literal, of `char` or of `char8_t`. Anything else is refused, and
//!   a wide string is refused as one.
//! * `btf_decl_tag` is about a declaration, so on a structure, a union or an enumeration it is
//!   ignored with gcc's warning that it does not apply to types.
//! * `btf_type_tag` is about the type a declaration has, and a function's type is not one it
//!   tags, so on a function it is ignored with gcc's warning that it does not apply to functions.
//! * A structure, a union or an enumeration is tagged only where it is defined, so a
//!   `btf_type_tag` that lands on one anywhere else is ignored with gcc's warning that it was
//!   applied after the definition, once for each tag and each declarator. gcc points at the name
//!   of the type and this at the tag.
//!
//! gcc reads the attributes at the start of a parenthesized declarator, `int (__user *fp)(int)`,
//! as on the function and ignores them with its warning. The parser has already put them on the
//! pointer, which is where they are read here, so that pointer is tagged and nothing is said.
//!
//! The arguments are checked once for every tag in the unit, before anything else reads it, so
//! a tag the checker has no other reason to visit, on a cast or a parameter, is checked all the
//! same. gcc checks the place first, and says only that a `btf_decl_tag` on a type does not
//! apply there whatever its argument is. Here a tag whose argument is wrong is refused for that,
//! wherever it is, and only one whose argument is right is held against its place.
//!
//! gcc reads a lone name as an expression and says first that nothing declares it, where
//! nothing does. Here a name is not looked up, and is refused only for not being a string.

use rucc_ast::{AttrArg, AttrList, AttrSyntax, Attribute, DeclSpecs, Derived, DerivedList};
use rucc_diag::{Diagnostic, Span};
use rucc_lex::Encoding;
use rucc_types::{EnumId, RecordId, TypeId, TypeKind};

use crate::check::Checker;
use crate::tast::TypeTags;

/// The code of the errors about a tag's argument.
const BTF_ARGUMENT: &str = "E0858";

/// The code of the warning about a `btf_type_tag` on a structure, a union or an enumeration
/// away from its definition.
const BTF_AFTER_DEFINITION: &str = "E0861";

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

    /// The `btf_type_tag`s on each level of the type a declaration gives what it declares, with
    /// gcc's warning for each one it ignores, which is one on a function or on a structure, a
    /// union or an enumeration.
    ///
    /// The specifiers, the steps of the declarator and the attributes after it are what the
    /// tags were written in, and the type is what the declaration made of them. A level whose
    /// tags are all ignored is not among those that come back.
    pub(in crate::check) fn btf_type_tags(
        &mut self,
        specs: &DeclSpecs,
        derived: DerivedList,
        after: AttrList,
        ty: TypeId,
    ) -> TypeTags {
        let ast = self.ast;
        let steps = &ast[derived];
        let base = u32::try_from(steps.len()).unwrap_or(u32::MAX);
        let right = |attr: &&Attribute| {
            self.btf_tag(attr) == Some("btf_type_tag") && self.btf_fault(attr).is_none()
        };
        // Each tag with the level it is on, in the order they were written.
        let mut written: Vec<(u32, &Attribute)> = Vec::new();
        for attr in ast[specs.attrs].iter().filter(right) {
            let typed = attr.syntax == AttrSyntax::Standard && attr.span.lo >= specs.first.lo;
            written.push((if typed { base } else { 0 }, attr));
        }
        for (level, step) in (0..base).rev().zip(steps.iter().rev()) {
            if let Derived::Pointer { attrs, .. } = *step {
                written.extend(ast[attrs].iter().filter(right).map(|attr| (level, attr)));
            }
        }
        written.extend(ast[after].iter().filter(right).map(|attr| (0, attr)));
        let mut tags = TypeTags::new();
        let mut at = ty;
        for level in 0..=base {
            if written.is_empty() {
                break;
            }
            let canonical = self.types.canonical(at);
            let kind = self.types.kind(canonical);
            let here: Vec<&Attribute> =
                written.iter().filter(|(on, _)| *on == level).map(|&(_, attr)| attr).collect();
            written.retain(|(on, _)| *on != level);
            match kind {
                _ if here.is_empty() => {}
                TypeKind::Function(_) => {
                    for attr in here {
                        let what = "'btf_type_tag' attribute does not apply to functions";
                        self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                    }
                }
                TypeKind::Record(_) | TypeKind::Enum(_) => {
                    let bare = self.types.unqualified(canonical);
                    let spelled = self.spell(bare);
                    for attr in here {
                        let what =
                            format!("ignoring attributes applied to '{spelled}' after definition");
                        let warning = Diagnostic::warning(what, attr.span);
                        self.report(warning.with_code(BTF_AFTER_DEFINITION));
                    }
                }
                _ => {
                    let strings = here.into_iter().filter_map(|attr| self.btf_string(attr));
                    tags.push((level, last_first(strings)));
                }
            }
            at = match kind {
                TypeKind::Pointer(to) => to,
                TypeKind::Array { elem, .. } => elem,
                TypeKind::Function(which) => self.types.signature(which).ret,
                _ => break,
            };
        }
        tags
    }

    /// Keeps the `btf_type_tag`s written after the body of the structure or union being
    /// defined, which tag the record itself.
    pub(in crate::check) fn btf_tagged_record(&mut self, record: RecordId, attrs: AttrList) {
        let tags = self.btf_strings(&[attrs], "btf_type_tag");
        self.tast.record_tagged_record(record, tags);
    }

    /// Keeps the `btf_type_tag`s written after the body of the enumeration being defined, which
    /// tag the enumeration itself.
    pub(in crate::check) fn btf_tagged_enum(&mut self, which: EnumId, attrs: AttrList) {
        let tags = self.btf_strings(&[attrs], "btf_type_tag");
        self.tast.record_tagged_enum(which, tags);
    }

    /// The strings of the `btf_decl_tag`s in the lists whose arguments are right, in the order
    /// gcc keeps them on the declaration: the lists are the attributes after the declarator and
    /// then the specifiers', gcc puts each attribute it reads on the front, and a tag the
    /// declaration already has is not put there again.
    pub(in crate::check) fn btf_decl_tags(&self, lists: &[AttrList]) -> Vec<Vec<u8>> {
        self.btf_strings(lists, "btf_decl_tag")
    }

    /// The strings of the tags of that name in the lists whose arguments are right, the last
    /// written first and each once.
    fn btf_strings(&self, lists: &[AttrList], name: &str) -> Vec<Vec<u8>> {
        let ast = self.ast;
        let written = lists.iter().flat_map(|&attrs| &ast[attrs]);
        let right = written.filter(|attr| {
            self.btf_tag(attr) == Some(name) && self.btf_fault(attr).is_none()
        });
        last_first(right.filter_map(|attr| self.btf_string(attr)))
    }

    /// The string a tag whose argument is right holds, up to its first zero, which is all of it
    /// a reader of the debugging information sees.
    fn btf_string(&self, attr: &Attribute) -> Option<Vec<u8>> {
        let ast = self.ast;
        let [AttrArg::Expr(expr)] = ast[attr.args][..] else { return None };
        let rucc_ast::Expr::Str(string) = ast[expr] else { return None };
        let elements = ast[string].elements.iter().take_while(|&&element| element != 0);
        Some(elements.map(|&element| element as u8).collect())
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

/// The strings in the order gcc chains them, which is the last written first, and each once: gcc
/// puts each attribute it reads on the front and does not put one there that is already there.
fn last_first(written: impl Iterator<Item = Vec<u8>>) -> Vec<Vec<u8>> {
    let mut tags: Vec<Vec<u8>> = Vec::new();
    for tag in written {
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags.reverse();
    tags
}
