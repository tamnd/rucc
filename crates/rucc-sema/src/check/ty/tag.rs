//! What a tag's body declares: the members of a `struct` or a `union`, and the enumerators of
//! an `enum`.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.1.
//!
//! This is the other half of the type builder, and it is a different job. The specifiers and the
//! declarator in the parent module build one type out of what one declaration wrote. A body
//! builds a type out of the declarations written inside it, so it walks the members, folds the
//! bit-field widths, hands the result to the layout in `rucc-types` and completes the type that
//! the tag has been naming since before the brace was opened.
//!
//! Every wording and every rule below was measured against gcc 13.3 on x86-64 Linux rather than
//! recalled, which is the same rule the layout itself follows.
//!
//! # A definition is not a mention
//!
//! `struct S *p;` refers to whatever `S` already means, anywhere on the scope stack. A
//! definition is different: it declares or completes in the scope it is written in, so what it
//! asks is [`Scopes::tag_here`](crate::Scopes::tag_here), and `struct S { int x; };` inside a
//! function defines a new type even where an outer `struct S` is visible. What that lookup finds
//! is the forward declaration this body completes, unless it finds a tag of another kind or a
//! definition already read. Those two are reported and answered with a declaration of their own,
//! so that the members of the second definition are still checked and the tag goes on meaning
//! the first.
//!
//! # What an enumeration is represented in
//!
//! Measured in both `-std=c17` and `-std=c2x`, which agree. Let the enumerator values run from a
//! least to a greatest. When the least is not negative the candidates are `unsigned int` and
//! then `unsigned long`, and otherwise they are `int` and then `long`, and the first candidate
//! holding both ends is the answer. So `enum { 1 }` is represented in `unsigned int`,
//! `enum { -1, 1 }` in `int`, `enum { -1, 0x80000000 }` in `long`, and
//! `enum { 0xffffffffffffffff }` in `unsigned long`.
//!
//! What type an enumerator itself has is a second question with a different answer. It is `int`
//! whenever the value fits in one, which is what makes the enumerators of `enum { 1 }` `int`
//! rather than `unsigned int`, and it is the underlying type otherwise. Where C23's underlying
//! type was written it is that type and none of the above applies.

use std::collections::HashSet;

use rucc_ast::{self as ast, Member, TypeSpec};
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{
    ArrayLen, EnumId, FieldDecl, IntKind, IntegerInfo, Layout, LayoutError, RecordError, RecordId,
    RecordKind, RecordLayout, RecordOptions, TypeId, TypeKind, integer_info, is_complete,
    is_function, is_void, layout_record,
};

use super::{MEMBER, Subject};
use crate::check::Checker;
use crate::scope::{Binding, Tag, TagKind};

/// What the tag of a definition turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Defining {
    /// The tag names a type declared in this scope and never given a body, which is the type
    /// this definition completes.
    Complete(TypeId),
    /// The tag names nothing in this scope, so this definition is what declares it.
    New,
    /// The tag names something a definition cannot fill in, which has been reported.
    Refused,
}

impl Checker<'_> {
    /// The record a definition fills in, and the type its tag names.
    pub(super) fn record_defined(
        &mut self,
        kind: RecordKind,
        tag: Option<Symbol>,
        tag_kind: TagKind,
        span: Span,
    ) -> (RecordId, TypeId) {
        let defining = self.defining(tag, tag_kind, span);
        if let Defining::Complete(ty) = defining {
            if let TypeKind::Record(id) = self.types.kind(self.types.canonical(ty)) {
                return (id, ty);
            }
        }
        let id = self.types.declare_record(kind, tag);
        let ty = self.types.record(id);
        if defining == Defining::New {
            if let Some(name) = tag {
                self.scopes.declare_tag(name, Tag { kind: tag_kind, ty });
            }
        }
        (id, ty)
    }

    /// The enumeration a definition fills in, and the type its tag names.
    pub(super) fn enum_defined(&mut self, tag: Option<Symbol>, span: Span) -> (EnumId, TypeId) {
        let defining = self.defining(tag, TagKind::Enum, span);
        if let Defining::Complete(ty) = defining {
            if let TypeKind::Enum(id) = self.types.kind(self.types.canonical(ty)) {
                return (id, ty);
            }
        }
        let id = self.types.declare_enum(tag);
        let ty = self.types.enumeration(id);
        if defining == Defining::New {
            if let Some(name) = tag {
                self.scopes.declare_tag(name, Tag { kind: TagKind::Enum, ty });
            }
        }
        (id, ty)
    }

    /// What the tag of a definition already means in the scope the definition is written in.
    fn defining(&mut self, tag: Option<Symbol>, kind: TagKind, span: Span) -> Defining {
        // No tag is nothing to look up. The type is reachable only through the declarators of
        // the one declaration that wrote it, and it is new every time.
        let Some(name) = tag else { return Defining::New };
        let Some(found) = self.scopes.tag_here(name) else { return Defining::New };
        if found.kind != kind {
            let spelled = self.text(name).to_owned();
            self.report(
                Diagnostic::error(format!("'{spelled}' defined as wrong kind of tag"), span)
                    .with_code("E0531"),
            );
            return Defining::Refused;
        }
        if self.built.defined.contains(&found.ty) {
            let spelled = self.text(name).to_owned();
            // gcc has two words for the one thing here and does not use them interchangeably:
            // a structure or a union is redefined and an enumeration is redeclared.
            let message = match kind {
                TagKind::Enum => format!("redeclaration of 'enum {spelled}'"),
                _ => format!("redefinition of '{} {spelled}'", kind.as_str()),
            };
            self.report(Diagnostic::error(message, span).with_code("E0561"));
            return Defining::Refused;
        }
        Defining::Complete(found.ty)
    }

    /// Reads the members of a `struct` or a `union` and lays them out.
    pub(super) fn record_body(
        &mut self,
        id: RecordId,
        kind: RecordKind,
        members: ast::MemberList,
        attrs: ast::AttrList,
        pack: Option<u32>,
        span: Span,
    ) {
        // Read before the members are, because `aligned(n)` folds an expression and the members
        // are held by a borrow of the tree while they are being walked.
        let packing = self.packing(attrs);
        // The tree outlives the checker's own borrows, so taking the reference out first is
        // what lets the walk below call methods that take the checker mutably.
        let ast = self.ast;
        let mut fields: Vec<(FieldDecl, Span)> = Vec::with_capacity(ast[members].len());
        let mut named = HashSet::new();
        for member in &ast[members] {
            let field = match *member {
                Member::Field(field) => field,
                Member::StaticAssert { span, .. } => {
                    self.unsupported_type("a static assertion among the members", span);
                    continue;
                }
            };
            let Some((decl, at)) = self.member_decl(field) else { continue };
            if let Some(name) = decl.name {
                if !named.insert(name) {
                    let spelled = self.text(name).to_owned();
                    self.report(
                        Diagnostic::error(format!("duplicate member '{spelled}'"), at)
                            .with_code("E0548"),
                    );
                    continue;
                }
            }
            fields.push((decl, at));
        }
        self.check_flexible(kind, &mut fields);

        let decls: Vec<FieldDecl> = fields.iter().map(|(decl, _)| *decl).collect();
        let options = RecordOptions {
            packed: packing.packed,
            align: packing.align.map(u64::from),
            pack: pack.map(u64::from),
        };
        let laid_out = match layout_record(&self.types, kind, &decls, &options, self.cx.target) {
            Ok(laid_out) => Some(laid_out),
            Err(error) => {
                self.record_error(id, &fields, error, span);
                None
            }
        };
        // A record whose members did not lay out is completed all the same, with nothing in it.
        // What is wrong with it has been reported once, and leaving it incomplete would report
        // it again at every use.
        let laid_out =
            laid_out.unwrap_or(RecordLayout { layout: Layout::new(0, 1), fields: Vec::new() });
        self.types.complete_record(id, laid_out);
    }

    /// One member, or nothing where what was written does not declare one.
    fn member_decl(&mut self, field: ast::Field) -> Option<(FieldDecl, Span)> {
        let (ty, subject) = self.member_type(field)?;
        let canonical = self.types.canonical(ty);
        let who = self.member_named(subject.name);
        if is_function(&self.types, canonical) {
            self.report(
                Diagnostic::error(format!("field {who} declared as a function"), subject.span)
                    .with_code("E0551"),
            );
            return None;
        }
        if is_void(&self.types, canonical) {
            self.report(
                Diagnostic::error(format!("variable or field {who} declared void"), subject.span)
                    .with_code("E0550"),
            );
            return None;
        }
        // An array with no size is either a flexible array member or an incomplete member like
        // any other, and which of the two it is depends on where it sits rather than on what it
        // is, so it is left for the pass that knows the answer.
        if !is_complete(&self.types, canonical) && !self.is_flexible(ty) {
            self.report(
                Diagnostic::error(format!("field {who} has incomplete type"), subject.span)
                    .with_code("E0549"),
            );
            return None;
        }
        let bits = match field.bits {
            Some(width) => Some(self.bit_width(ty, width, subject)?),
            None => None,
        };
        // A member carries its own `packed` and `aligned`, which are not the record's: `packed`
        // on one member takes the padding out in front of that member alone, and `aligned` on
        // one raises where it sits. GCC takes them on the member's declaration and on its
        // specifiers alike, and a member has one list of each.
        let mut packing = self.packing(field.attrs);
        let specs = self.ast[field.specs];
        let on_specs = self.packing(specs.attrs);
        packing.packed |= on_specs.packed;
        packing.align = packing.align.max(on_specs.align);
        packing.align = packing.align.max(self.member_alignas(specs.align, subject.span));
        let decl = FieldDecl {
            name: subject.name,
            ty,
            bits,
            align: packing.align.map(u64::from),
            packed: packing.packed,
        };
        Some((decl, subject.span))
    }

    /// The type of one member, and who a diagnostic about it names.
    fn member_type(&mut self, field: ast::Field) -> Option<(TypeId, Subject)> {
        let Some(declarator) = field.declarator else {
            let subject = Subject { name: None, span: field.span };
            // A member with no declarator is an unnamed bit-field, or an anonymous member,
            // which is a `struct` or a `union` with neither tag nor name and whose members are
            // reached as if they were written here. Anything else declares nothing at all,
            // which gcc warns about and accepts.
            let anonymous = matches!(
                self.ast[field.specs].ty,
                TypeSpec::Record { tag: None, fields: Some(_), .. }
            );
            if field.bits.is_none() && !anonymous {
                self.report(
                    Diagnostic::warning(
                        "declaration does not declare anything".to_string(),
                        field.span,
                    )
                    .with_code("E0547"),
                );
                return None;
            }
            return Some((self.specified_type(field.specs, subject, MEMBER), subject));
        };
        let node = self.ast[declarator];
        let span = if node.name.is_some() { node.name_span } else { field.span };
        let subject = Subject { name: node.name, span };
        Some((self.build_type(field.specs, declarator, MEMBER), subject))
    }

    /// The width of a bit-field, folded and measured against the type it was declared in.
    fn bit_width(&mut self, ty: TypeId, width: ast::ExprId, subject: Subject) -> Option<u32> {
        let who = self.member_named(subject.name);
        let value = self.expr(width);
        let span = self.tast.expr_span(value);
        if self.is_poisoned(value) {
            return None;
        }
        let Ok(bits) = self.eval_integer(value) else {
            self.report(
                Diagnostic::error(format!("bit-field {who} width not an integer constant"), span)
                    .with_code("E0555"),
            );
            return None;
        };
        // The type has to be one a run of bits is a value of. The standard allows `_Bool`,
        // `int` and `unsigned int`, and both compilers take every integer type, which is what
        // every header that declares a `long` bit-field relies on.
        let Some(info) = integer_info(&self.types, ty, self.cx.target) else {
            self.report(
                Diagnostic::error(format!("bit-field {who} has invalid type"), subject.span)
                    .with_code("E0556"),
            );
            return None;
        };
        if bits < 0 {
            self.report(
                Diagnostic::error(format!("negative width in bit-field {who}"), span)
                    .with_code("E0558"),
            );
            return None;
        }
        // Zero is the width that means the next member starts at the next boundary, and it says
        // nothing about a member of its own, so a name on it has nothing to name.
        if bits == 0 && subject.name.is_some() {
            self.report(
                Diagnostic::error(format!("zero width for bit-field {who}"), span)
                    .with_code("E0557"),
            );
            return None;
        }
        let bits = u32::try_from(bits).unwrap_or(u32::MAX);
        // Against the width of the value rather than of the object, which is what makes
        // `_Bool b : 2` too wide while its storage is eight bits.
        if bits > info.width {
            self.report(
                Diagnostic::error(format!("width of {who} exceeds its type"), subject.span)
                    .with_code("E0559"),
            );
            // The width its type has, which is what gcc lays the member out at once it has said
            // so, and which keeps the rest of the record where the program expects it.
            return Some(info.width);
        }
        Some(bits)
    }

    /// The rules about a member with no size, which are about where it sits rather than about
    /// what it is.
    ///
    /// A flexible array member is the last member of a `struct` that has other named members. A
    /// member with no size anywhere else is refused, and dropped, since the layout has no offset
    /// to give it and every member after it would be at the wrong one.
    fn check_flexible(&mut self, kind: RecordKind, fields: &mut Vec<(FieldDecl, Span)>) {
        let last = fields.len().wrapping_sub(1);
        let mut refused = Vec::new();
        for (index, (decl, span)) in fields.iter().enumerate() {
            if !self.is_flexible(decl.ty) {
                continue;
            }
            let named = fields
                .iter()
                .enumerate()
                .any(|(other, (decl, _))| other != index && decl.name.is_some());
            let wrong = if kind == RecordKind::Union {
                Some(("flexible array member in union", "E0552"))
            } else if index != last {
                Some(("flexible array member not at end of struct", "E0553"))
            } else if !named {
                Some(("flexible array member in a struct with no named members", "E0554"))
            } else {
                None
            };
            if let Some((message, code)) = wrong {
                refused.push((index, message, code, *span));
            }
        }
        for &(_, message, code, span) in &refused {
            self.report(Diagnostic::error(message.to_string(), span).with_code(code));
        }
        for &(index, ..) in refused.iter().rev() {
            fields.remove(index);
        }
    }

    /// Whether a type is an array whose size was left out.
    fn is_flexible(&self, ty: TypeId) -> bool {
        let kind = self.types.kind(self.types.canonical(ty));
        matches!(kind, TypeKind::Array { len: ArrayLen::Unknown, .. })
    }

    /// A record the layout refused, which is a member with no layout of its own.
    fn record_error(
        &mut self,
        id: RecordId,
        fields: &[(FieldDecl, Span)],
        error: RecordError,
        span: Span,
    ) {
        // The index is into the declarations that were handed over, which are these.
        let (index, what, code) = match error {
            RecordError::TooLarge => {
                self.record_too_large(id, span);
                return;
            }
            RecordError::Member { index, error: LayoutError::TooLarge } => {
                (index, "is too large", "E0560")
            }
            RecordError::Member { index, .. } => (index, "has incomplete type", "E0549"),
            RecordError::BitFieldTooWide { index, .. } => (index, "exceeds its type", "E0559"),
        };
        let Some(&(decl, at)) = fields.get(index) else { return };
        let who = self.member_named(decl.name);
        self.report(Diagnostic::error(format!("field {who} {what}"), at).with_code(code));
    }

    /// A record larger than an object may be.
    fn record_too_large(&mut self, id: RecordId, span: Span) {
        let ty = self.types.record(id);
        let spelled = self.spell(ty);
        self.report(
            Diagnostic::error(format!("type '{spelled}' is too large"), span).with_code("E0560"),
        );
    }

    /// `'x'`, or what gcc calls a member that has no name.
    fn member_named(&self, name: Option<Symbol>) -> String {
        match name {
            Some(name) => format!("'{}'", self.text(name)),
            None => "'<anonymous>'".to_string(),
        }
    }

    /// Reads the enumerators, puts each of them in scope and decides what the enumeration is
    /// represented in.
    pub(super) fn enum_body(
        &mut self,
        id: EnumId,
        list: ast::EnumeratorList,
        fixed: Option<TypeId>,
        span: Span,
    ) {
        let ast = self.ast;
        let int = self.int();
        if ast[list].is_empty() {
            self.report(
                Diagnostic::error("empty enum is invalid".to_string(), span).with_code("E0562"),
            );
            self.types.complete_enum(id, fixed.unwrap_or(int), fixed.is_some());
            return;
        }

        let (low, high) = self.enum_bounds(fixed);
        // The type an enumerator has while the list is still being read, which is what an
        // enumerator referring to an earlier one of the same enumeration is folded with. It is
        // the final answer when the program wrote the underlying type, and a placeholder
        // otherwise, since what an enumeration is represented in is not known until its last
        // enumerator has been seen.
        let provisional = fixed.unwrap_or(int);
        let mut values = Vec::with_capacity(ast[list].len());
        let mut next = Some(0i128);
        for enumerator in &ast[list] {
            let value = match enumerator.value {
                Some(expr) => {
                    let value = self
                        .enumerator_value(enumerator.name, expr)
                        .unwrap_or_else(|| next.unwrap_or(high));
                    self.check_enum_range(value, fixed, (low, high), enumerator.span)
                }
                // The enumerator after the greatest value there is has nowhere to go, which is
                // what `enum E : unsigned char { A = 255, B };` asks for and gcc refuses.
                None => match next {
                    Some(value) => value,
                    None => {
                        self.report(
                            Diagnostic::error(
                                "overflow in enumeration values".to_string(),
                                enumerator.span,
                            )
                            .with_code("E0566"),
                        );
                        high
                    }
                },
            };
            self.declare_enumerator(enumerator.name, value, provisional, enumerator.span);
            values.push((enumerator.name, value, enumerator.span));
            next = if value < high { Some(value + 1) } else { None };
        }

        let underlying = match fixed {
            Some(ty) => ty,
            None => self.enum_underlying(&values),
        };
        self.types.complete_enum(id, underlying, fixed.is_some());

        // Declared a second time, now that there is an answer to what type an enumerator has.
        // Nothing has been folded with the placeholder in between, since an enumerator is the
        // only thing that can refer to an earlier enumerator of the same enumeration and every
        // one of them holds a value rather than a type.
        let (int_low, int_high) = self.enum_bounds(Some(int));
        for &(name, value, _) in &values {
            let ty = match fixed {
                Some(ty) => ty,
                None if value >= int_low && value <= int_high => int,
                None => underlying,
            };
            self.scopes.declare(name, Binding::Enumerator { value, ty });
        }
    }

    /// An enumerator the program gave a value, measured against what the enumeration can hold.
    fn check_enum_range(
        &mut self,
        value: i128,
        fixed: Option<TypeId>,
        range: (i128, i128),
        span: Span,
    ) -> i128 {
        let (low, high) = range;
        if value >= low && value <= high {
            return value;
        }
        let message = match fixed {
            Some(_) => "enumerator value outside the range of underlying type".to_string(),
            // Without an underlying type written the bound is the widest an enumeration is
            // represented in, which is the widest integer type there is and which gcc names in
            // the message rather than naming the enumeration.
            None => {
                let what = if value < 0 { "intmax_t" } else { "uintmax_t" };
                format!("enumerator value outside the range of '{what}'")
            }
        };
        self.report(Diagnostic::error(message, span).with_code("E0565"));
        value.clamp(low, high)
    }

    /// The value an `= expression` gives an enumerator.
    fn enumerator_value(&mut self, name: Symbol, expr: ast::ExprId) -> Option<i128> {
        let value = self.expr(expr);
        let span = self.tast.expr_span(value);
        if self.is_poisoned(value) {
            return None;
        }
        match self.eval_integer(value) {
            Ok(value) => Some(value),
            Err(failure) => {
                if !failure.poisoned {
                    let spelled = self.text(name).to_owned();
                    let message =
                        format!("enumerator value for '{spelled}' is not an integer constant");
                    self.report(Diagnostic::error(message, span).with_code("E0564"));
                }
                None
            }
        }
    }

    /// Puts one enumerator in scope, reporting a name that is taken already.
    fn declare_enumerator(&mut self, name: Symbol, value: i128, ty: TypeId, span: Span) {
        if let Some(binding) = self.scopes.lookup_here(name) {
            let spelled = self.text(name).to_owned();
            let message = match binding {
                Binding::Enumerator { .. } => format!("redeclaration of enumerator '{spelled}'"),
                _ => format!("'{spelled}' redeclared as different kind of symbol"),
            };
            self.report(Diagnostic::error(message, span).with_code("E0563"));
        }
        self.scopes.declare(name, Binding::Enumerator { value, ty });
    }

    /// The type the enumerators are represented in, where the program did not say.
    fn enum_underlying(&mut self, values: &[(Symbol, i128, Span)]) -> TypeId {
        let low = values.iter().map(|&(_, value, _)| value).min().unwrap_or(0);
        let high = values.iter().map(|&(_, value, _)| value).max().unwrap_or(0);
        let candidates =
            if low >= 0 { [IntKind::UInt, IntKind::ULong] } else { [IntKind::Int, IntKind::Long] };
        let mut chosen = self.types.int(candidates[1]);
        for kind in candidates {
            let ty = self.types.int(kind);
            let (least, greatest) = self.enum_bounds(Some(ty));
            if low >= least && high <= greatest {
                chosen = ty;
                break;
            }
        }
        chosen
    }

    /// The values an enumerator may take, which is the underlying type's range where the
    /// program wrote one and the widest range an enumeration is ever represented in otherwise.
    fn enum_bounds(&mut self, fixed: Option<TypeId>) -> (i128, i128) {
        if let Some(ty) = fixed {
            let Some(info) = integer_info(&self.types, ty, self.cx.target) else {
                // Not an integer type, which has been reported where it was written.
                return (i128::MIN, i128::MAX);
            };
            return bounds(info);
        }
        let signed = self.types.int(IntKind::Long);
        let unsigned = self.types.int(IntKind::ULong);
        let low = match integer_info(&self.types, signed, self.cx.target) {
            Some(info) => bounds(info).0,
            None => i128::MIN,
        };
        let high = match integer_info(&self.types, unsigned, self.cx.target) {
            Some(info) => bounds(info).1,
            None => i128::MAX,
        };
        (low, high)
    }
}

/// The least and the greatest value an integer type holds.
fn bounds(info: IntegerInfo) -> (i128, i128) {
    // A hundred and twenty eight bits is as wide as the folding goes, so there is no value of
    // the type that is not a value of an `i128` and no bound to compare against.
    if info.width >= 128 {
        return if info.signed { (i128::MIN, i128::MAX) } else { (0, i128::MAX) };
    }
    if info.signed {
        let high = (1i128 << (info.width - 1)) - 1;
        (-high - 1, high)
    } else {
        (0, (1i128 << info.width) - 1)
    }
}

/// The bodies, against the layout the members are handed to and the scope the enumerators go in.
#[cfg(test)]
mod tests {
    use rucc_ast::{BuiltinSet, DeclSpecsId, DeclaratorId, Derived, Quals, UnaryOp};
    use rucc_lex::{IntConstant, IntConstantType, Remarks};

    use super::*;
    use crate::check::ty::tests::{Fixture, message, messages, spelled};

    /// An ordinary member declaring one name.
    fn member(specs: DeclSpecsId, declarator: DeclaratorId) -> Member {
        Member::Field(ast::Field {
            specs,
            declarator: Some(declarator),
            bits: None,
            attrs: ast::AttrList::EMPTY,
            span: Span::DUMMY,
        })
    }

    /// A bit-field, named or not, of the given width.
    fn bit_field(
        specs: DeclSpecsId,
        declarator: Option<DeclaratorId>,
        bits: ast::ExprId,
    ) -> Member {
        Member::Field(ast::Field {
            specs,
            declarator,
            bits: Some(bits),
            attrs: ast::AttrList::EMPTY,
            span: Span::DUMMY,
        })
    }

    /// A member with neither a declarator nor a width, which is an anonymous member when the
    /// specifiers are a record with a body and nothing at all otherwise.
    fn bare(specs: DeclSpecsId) -> Member {
        Member::Field(ast::Field {
            specs,
            declarator: None,
            bits: None,
            attrs: ast::AttrList::EMPTY,
            span: Span::DUMMY,
        })
    }

    /// The specifiers of a `struct` or a `union` definition.
    fn record(
        fixture: &mut Fixture,
        kind: ast::RecordKind,
        tag: Option<&str>,
        members: &[Member],
    ) -> DeclSpecsId {
        let tag = tag.map(|text| fixture.name(text));
        let fields = Some(fixture.ast.add_member_list(members));
        fixture.specs(
            TypeSpec::Record { kind, tag, fields, attrs: ast::AttrList::EMPTY, pack: None },
            Quals::NONE,
        )
    }

    /// The specifiers of a `struct` definition, which is what most of these are.
    fn structure(fixture: &mut Fixture, tag: Option<&str>, members: &[Member]) -> DeclSpecsId {
        record(fixture, ast::RecordKind::Struct, tag, members)
    }

    /// One enumerator, with a value or without one.
    fn enumerator(
        fixture: &mut Fixture,
        name: &str,
        value: Option<ast::ExprId>,
    ) -> ast::Enumerator {
        let name = fixture.name(name);
        ast::Enumerator { name, value, attrs: ast::AttrList::EMPTY, span: Span::DUMMY }
    }

    /// The specifiers of an `enum` definition.
    fn enumeration(
        fixture: &mut Fixture,
        tag: Option<&str>,
        underlying: Option<ast::TypeNameId>,
        enumerators: &[ast::Enumerator],
    ) -> DeclSpecsId {
        let tag = tag.map(|text| fixture.name(text));
        let enumerators = Some(fixture.ast.add_enumerator_list(enumerators));
        fixture.specs(
            TypeSpec::Enum { tag, enumerators, underlying, attrs: ast::AttrList::EMPTY },
            Quals::NONE,
        )
    }

    /// An integer constant of the type the lexer would have given it.
    fn constant(fixture: &mut Fixture, value: u128, kind: IntKind) -> ast::ExprId {
        let ty = IntConstantType::Standard(kind);
        let id = fixture.ast.add_int(IntConstant { value, ty, remarks: Remarks::default() });
        fixture.ast.expr(ast::Expr::Int(id), Span::DUMMY)
    }

    /// The same, negated, which is the only way to write a negative one.
    fn negative(fixture: &mut Fixture, value: u128) -> ast::ExprId {
        let operand = constant(fixture, value, IntKind::Int);
        fixture.ast.expr(ast::Expr::Unary { op: UnaryOp::Minus, operand }, Span::DUMMY)
    }

    /// The type a definition on its own declares, with no declarator after it.
    fn defined(fixture: &mut Fixture, specs: DeclSpecsId) -> DeclaratorId {
        let _ = specs;
        fixture.declarator(None, &[])
    }

    /// The size and the alignment of a record, and where its members were placed, in bits.
    fn placed(checker: &Checker<'_>, ty: TypeId) -> (u64, u64, Vec<u128>) {
        let TypeKind::Record(id) = checker.types.kind(checker.types.canonical(ty)) else {
            panic!("a record type");
        };
        let info = checker.types.record_info(id);
        let layout = info.layout.expect("a record the definition completed");
        let offsets = info.fields.iter().map(rucc_types::Field::bit_offset).collect();
        (layout.size, layout.align, offsets)
    }

    /// What an enumeration turned out to be represented in.
    fn underlying(checker: &Checker<'_>, ty: TypeId) -> String {
        let TypeKind::Enum(id) = checker.types.kind(checker.types.canonical(ty)) else {
            panic!("an enumeration type");
        };
        let underlying = checker.types.enum_info(id).underlying.expect("a complete enumeration");
        spelled(checker, underlying)
    }

    /// The value and the type an enumerator was declared with.
    fn enumerator_binding(checker: &Checker<'_>, name: Symbol) -> (i128, String) {
        let Some(Binding::Enumerator { value, ty }) = checker.scopes.lookup(name) else {
            panic!("an enumerator in scope");
        };
        (value, spelled(checker, ty))
    }

    #[test]
    fn a_body_lays_the_members_out_and_completes_the_type_the_tag_names() {
        let mut fixture = Fixture::new();
        let int = fixture.int_specs();
        let ch = fixture.keywords(&[BuiltinSet::CHAR]);
        let x = fixture.declarator(Some("x"), &[]);
        let c = fixture.declarator(Some("c"), &[]);
        let specs = structure(&mut fixture, Some("S"), &[member(ch, c), member(int, x)]);
        let hole = defined(&mut fixture, specs);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(specs, hole);
        assert_eq!(spelled(&checker, ty), "struct S");
        assert!(is_complete(&checker.types, ty));
        // The `char` at zero and the `int` at its own alignment, which is four bytes in.
        assert_eq!(placed(&checker, ty), (8, 4, vec![0, 32]));
        assert!(messages(&checker).is_empty());
    }

    #[test]
    fn a_deduced_type_on_a_member_names_nothing_and_says_where_it_was_written() {
        let mut fixture = Fixture::new();
        // A member has no initializer to deduce from, so the message names the member rather
        // than the prototype the other one names.
        let deduced = fixture.specs(TypeSpec::Auto(ast::Deduction::AutoType), Quals::NONE);
        let m = fixture.declarator(Some("m"), &[]);
        let specs = structure(&mut fixture, Some("S"), &[member(deduced, m)]);
        let hole = defined(&mut fixture, specs);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(specs, hole);
        assert_eq!(spelled(&checker, ty), "struct S");
        assert_eq!(message(&checker), "'__auto_type' not allowed in struct member");
    }

    #[test]
    fn a_union_is_as_large_as_its_largest_member_and_every_member_is_at_zero() {
        let mut fixture = Fixture::new();
        let int = fixture.int_specs();
        let ch = fixture.keywords(&[BuiltinSet::CHAR]);
        let x = fixture.declarator(Some("x"), &[]);
        let c = fixture.declarator(Some("c"), &[]);
        let specs = record(
            &mut fixture,
            ast::RecordKind::Union,
            Some("U"),
            &[member(int, x), member(ch, c)],
        );
        let hole = defined(&mut fixture, specs);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(specs, hole);
        assert_eq!(spelled(&checker, ty), "union U");
        assert_eq!(placed(&checker, ty), (4, 4, vec![0, 0]));
        assert!(messages(&checker).is_empty());
    }

    #[test]
    fn the_tag_is_bound_before_the_members_so_a_structure_can_point_at_itself() {
        let mut fixture = Fixture::new();
        let tag = fixture.name("S");
        let inner = fixture.specs(
            TypeSpec::Record {
                kind: ast::RecordKind::Struct,
                tag: Some(tag),
                fields: None,
                attrs: ast::AttrList::EMPTY,
                pack: None,
            },
            Quals::NONE,
        );
        let next = fixture.declarator(
            Some("next"),
            &[Derived::Pointer { quals: Quals::NONE, attrs: ast::AttrList::EMPTY }],
        );
        let specs = structure(&mut fixture, Some("S"), &[member(inner, next)]);
        let hole = defined(&mut fixture, specs);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(specs, hole);
        assert_eq!(placed(&checker, ty), (8, 8, vec![0]));
        assert!(messages(&checker).is_empty());
        // One structure and not two, which is what makes the member point at the type it is a
        // member of.
        let TypeKind::Record(id) = checker.types.kind(ty) else { panic!("a record type") };
        let member = checker.types.record_info(id).fields[0].ty;
        assert_eq!(spelled(&checker, member), "struct S *");
    }

    #[test]
    fn a_member_declared_twice_is_reported_once_and_left_out() {
        let mut fixture = Fixture::new();
        let int = fixture.int_specs();
        let first = fixture.declarator(Some("x"), &[]);
        let again = fixture.declarator(Some("x"), &[]);
        let specs = structure(&mut fixture, Some("S"), &[member(int, first), member(int, again)]);
        let hole = defined(&mut fixture, specs);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(specs, hole);
        assert_eq!(message(&checker), "duplicate member 'x'");
        assert_eq!(placed(&checker, ty), (4, 4, vec![0]));
    }

    #[test]
    fn a_member_that_declares_no_name_declares_a_type_or_nothing_at_all() {
        let mut fixture = Fixture::new();
        let int = fixture.int_specs();
        let q = fixture.declarator(Some("q"), &[]);
        let anonymous = structure(&mut fixture, None, &[member(int, q)]);
        let nothing = fixture.int_specs();
        let specs = structure(&mut fixture, Some("S"), &[bare(anonymous), bare(nothing)]);
        let hole = defined(&mut fixture, specs);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(specs, hole);
        // The anonymous member is a member, and the `int;` beside it is a declaration that
        // declares nothing, which gcc warns about and goes on from.
        assert_eq!(message(&checker), "declaration does not declare anything");
        assert_eq!(placed(&checker, ty), (4, 4, vec![0]));
    }

    #[test]
    fn a_member_of_a_type_that_has_no_size_is_reported_by_what_is_wrong_with_it() {
        let mut fixture = Fixture::new();
        let int = fixture.int_specs();
        let void = fixture.keywords(&[BuiltinSet::VOID]);
        let tag = fixture.name("T");
        let incomplete = fixture.specs(
            TypeSpec::Record {
                kind: ast::RecordKind::Struct,
                tag: Some(tag),
                fields: None,
                attrs: ast::AttrList::EMPTY,
                pack: None,
            },
            Quals::NONE,
        );
        let v = fixture.declarator(Some("v"), &[]);
        let t = fixture.declarator(Some("t"), &[]);
        let params = fixture.ast.add_param_list(&[]);
        let call = Derived::Function { params, variadic: false, kind: ast::ParamKind::Void };
        let f = fixture.declarator(Some("f"), &[call]);
        let x = fixture.declarator(Some("x"), &[]);
        let members = [member(void, v), member(incomplete, t), member(int, f), member(int, x)];
        let specs = structure(&mut fixture, Some("S"), &members);
        let hole = defined(&mut fixture, specs);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(specs, hole);
        assert_eq!(
            messages(&checker),
            [
                "variable or field 'v' declared void",
                "field 't' has incomplete type",
                "field 'f' declared as a function",
            ]
        );
        // The one member that does have a size is still laid out, which is what keeps a
        // declaration that follows this one from being wrong about every offset in it.
        assert_eq!(placed(&checker, ty), (4, 4, vec![0]));
    }

    #[test]
    fn a_bit_field_is_folded_and_packed_against_the_width_of_its_own_type() {
        let mut fixture = Fixture::new();
        let int = fixture.int_specs();
        let c = fixture.declarator(Some("c"), &[]);
        let b = fixture.declarator(Some("b"), &[]);
        let ch = fixture.keywords(&[BuiltinSet::CHAR]);
        let thirty = fixture.int(30);
        let members = [member(ch, c), bit_field(int, Some(b), thirty)];
        let specs = structure(&mut fixture, Some("S"), &members);
        let hole = defined(&mut fixture, specs);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(specs, hole);
        // Thirty bits do not fit in what is left of the first four bytes, so the field starts
        // at the next boundary of its own type.
        assert_eq!(placed(&checker, ty), (8, 4, vec![0, 32]));
        assert!(messages(&checker).is_empty());
    }

    #[test]
    fn a_bit_field_width_is_measured_and_a_name_on_a_zero_width_one_has_nothing_to_name() {
        let mut fixture = Fixture::new();
        let int = fixture.int_specs();
        let double = fixture.keywords(&[BuiltinSet::DOUBLE]);
        let zero = fixture.int(0);
        let one = fixture.int(1);
        let too_wide = fixture.int(64);
        let below = negative(&mut fixture, 1);
        let x = fixture.declarator(Some("x"), &[]);
        let w = fixture.declarator(Some("w"), &[]);
        let f = fixture.declarator(Some("f"), &[]);
        let members = [
            bit_field(int, Some(x), zero),
            bit_field(int, None, below),
            bit_field(int, Some(w), too_wide),
            bit_field(double, Some(f), one),
        ];
        let specs = structure(&mut fixture, Some("S"), &members);
        let hole = defined(&mut fixture, specs);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(specs, hole);
        assert_eq!(
            messages(&checker),
            [
                "zero width for bit-field 'x'",
                "negative width in bit-field '<anonymous>'",
                "width of 'w' exceeds its type",
                "bit-field 'f' has invalid type",
            ]
        );
        // The one that was too wide is kept at the width its type has, which is where gcc
        // leaves it and what keeps the members after it where the program put them.
        assert_eq!(placed(&checker, ty), (4, 4, vec![0]));
    }

    #[test]
    fn an_unnamed_zero_width_bit_field_moves_the_next_member_on_and_names_nothing() {
        let mut fixture = Fixture::new();
        let int = fixture.int_specs();
        let ch = fixture.keywords(&[BuiltinSet::CHAR]);
        let zero = fixture.int(0);
        let c = fixture.declarator(Some("c"), &[]);
        let d = fixture.declarator(Some("d"), &[]);
        let members = [member(ch, c), bit_field(int, None, zero), member(ch, d)];
        let specs = structure(&mut fixture, Some("S"), &members);
        let hole = defined(&mut fixture, specs);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(specs, hole);
        assert_eq!(placed(&checker, ty), (5, 1, vec![0, 32, 32]));
        assert!(messages(&checker).is_empty());
    }

    #[test]
    fn a_flexible_array_member_is_the_last_member_of_a_struct_that_has_others() {
        let mut fixture = Fixture::new();
        let int = fixture.int_specs();
        let unsized_array = Derived::Array {
            size: ast::ArraySize::Unspecified,
            quals: Quals::NONE,
            has_static: false,
        };
        let x = fixture.declarator(Some("x"), &[]);
        let a = fixture.declarator(Some("a"), &[unsized_array]);
        let good = structure(&mut fixture, Some("S"), &[member(int, x), member(int, a)]);
        let alone = structure(&mut fixture, Some("T"), &[member(int, a)]);
        let inside = record(&mut fixture, ast::RecordKind::Union, Some("U"), &[member(int, a)]);
        let early = structure(&mut fixture, Some("V"), &[member(int, a), member(int, x)]);
        let hole = fixture.declarator(None, &[]);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(good, hole);
        // No size of its own, and the offset it would have had, which is what makes
        // `malloc(sizeof(struct S) + n)` the idiom it is.
        assert_eq!(placed(&checker, ty), (4, 4, vec![0, 32]));
        assert!(messages(&checker).is_empty());

        checker.declared_type(alone, hole);
        checker.declared_type(inside, hole);
        checker.declared_type(early, hole);
        assert_eq!(
            messages(&checker),
            [
                "flexible array member in a struct with no named members",
                "flexible array member in union",
                "flexible array member not at end of struct",
            ]
        );
    }

    #[test]
    fn a_definition_completes_the_tag_that_was_declared_before_it_and_refuses_a_second() {
        let mut fixture = Fixture::new();
        let tag = fixture.name("S");
        let forward = fixture.specs(
            TypeSpec::Record {
                kind: ast::RecordKind::Struct,
                tag: Some(tag),
                fields: None,
                attrs: ast::AttrList::EMPTY,
                pack: None,
            },
            Quals::NONE,
        );
        let int = fixture.int_specs();
        let x = fixture.declarator(Some("x"), &[]);
        let y = fixture.declarator(Some("y"), &[]);
        let definition = structure(&mut fixture, Some("S"), &[member(int, x)]);
        let again = structure(&mut fixture, Some("S"), &[member(int, y)]);
        let hole = fixture.declarator(None, &[]);

        let mut checker = fixture.checker();
        let declared = checker.declared_type(forward, hole);
        assert!(!is_complete(&checker.types, declared));
        let defined = checker.declared_type(definition, hole);
        assert_eq!(defined, declared);
        assert!(is_complete(&checker.types, declared));
        assert!(messages(&checker).is_empty());

        // The second body is reported and checked, and the tag goes on meaning the first.
        checker.declared_type(again, hole);
        assert_eq!(message(&checker), "redefinition of 'struct S'");
        assert_eq!(checker.declared_type(forward, hole), declared);
    }

    #[test]
    fn a_body_written_against_a_tag_of_another_kind_is_reported_and_bound_to_nothing() {
        let mut fixture = Fixture::new();
        let int = fixture.int_specs();
        let x = fixture.declarator(Some("x"), &[]);
        let y = fixture.declarator(Some("y"), &[]);
        let structure_specs = structure(&mut fixture, Some("S"), &[member(int, x)]);
        let union_specs =
            record(&mut fixture, ast::RecordKind::Union, Some("S"), &[member(int, y)]);
        let hole = fixture.declarator(None, &[]);

        let mut checker = fixture.checker();
        let first = checker.declared_type(structure_specs, hole);
        let wrong = checker.declared_type(union_specs, hole);
        assert_eq!(message(&checker), "'S' defined as wrong kind of tag");
        assert_ne!(wrong, first);
        // The members of the refused definition are still laid out, since a second diagnostic
        // about each of them would be about a mistake the program did not make.
        assert_eq!(placed(&checker, wrong), (4, 4, vec![0]));
    }

    #[test]
    fn the_underlying_type_is_the_first_candidate_that_holds_every_enumerator() {
        let mut fixture = Fixture::new();
        let positive = constant(&mut fixture, 1, IntKind::Int);
        let below = negative(&mut fixture, 1);
        let unsigned_max = constant(&mut fixture, 4_294_967_295, IntKind::UInt);
        let signed_max = constant(&mut fixture, 9_223_372_036_854_775_807, IntKind::Long);
        let one = [enumerator(&mut fixture, "a", Some(positive))];
        let both = [
            enumerator(&mut fixture, "b", Some(below)),
            enumerator(&mut fixture, "c", Some(positive)),
        ];
        let wide = [
            enumerator(&mut fixture, "d", Some(below)),
            enumerator(&mut fixture, "e", Some(unsigned_max)),
        ];
        let widest =
            [enumerator(&mut fixture, "f", Some(signed_max)), enumerator(&mut fixture, "g", None)];
        let cases = [
            (enumeration(&mut fixture, None, None, &one), "unsigned int"),
            (enumeration(&mut fixture, None, None, &both), "int"),
            (enumeration(&mut fixture, None, None, &wide), "long"),
            (enumeration(&mut fixture, None, None, &widest), "unsigned long"),
        ];
        let hole = fixture.declarator(None, &[]);

        let mut checker = fixture.checker();
        for (specs, expected) in cases {
            let ty = checker.declared_type(specs, hole);
            assert_eq!(underlying(&checker, ty), expected);
        }
        assert!(messages(&checker).is_empty());
    }

    #[test]
    fn an_enumerator_is_an_int_wherever_the_value_fits_in_one() {
        let mut fixture = Fixture::new();
        let small = constant(&mut fixture, 1, IntKind::Int);
        let large = constant(&mut fixture, 4_294_967_295, IntKind::UInt);
        let enumerators = [
            enumerator(&mut fixture, "small", Some(small)),
            enumerator(&mut fixture, "large", Some(large)),
        ];
        let specs = enumeration(&mut fixture, Some("E"), None, &enumerators);
        let hole = fixture.declarator(None, &[]);
        let small_name = fixture.name("small");
        let large_name = fixture.name("large");

        let mut checker = fixture.checker();
        let ty = checker.declared_type(specs, hole);
        assert_eq!(underlying(&checker, ty), "unsigned int");
        assert_eq!(enumerator_binding(&checker, small_name), (1, "int".to_string()));
        // Which does not fit in an `int`, so it is kept in what the enumeration is kept in.
        assert_eq!(
            enumerator_binding(&checker, large_name),
            (4_294_967_295, "unsigned int".to_string())
        );
        assert!(messages(&checker).is_empty());
    }

    #[test]
    fn an_enumerator_with_no_value_is_one_more_than_the_one_before_it() {
        let mut fixture = Fixture::new();
        let five = constant(&mut fixture, 5, IntKind::Int);
        let enumerators = [
            enumerator(&mut fixture, "a", None),
            enumerator(&mut fixture, "b", Some(five)),
            enumerator(&mut fixture, "c", None),
        ];
        let specs = enumeration(&mut fixture, Some("E"), None, &enumerators);
        let hole = fixture.declarator(None, &[]);
        let a = fixture.name("a");
        let b = fixture.name("b");
        let c = fixture.name("c");

        let mut checker = fixture.checker();
        checker.declared_type(specs, hole);
        assert_eq!(enumerator_binding(&checker, a).0, 0);
        assert_eq!(enumerator_binding(&checker, b).0, 5);
        assert_eq!(enumerator_binding(&checker, c).0, 6);
        assert!(messages(&checker).is_empty());
    }

    #[test]
    fn the_underlying_type_the_program_wrote_is_what_the_enumerators_have_to_fit_in() {
        let mut fixture = Fixture::new();
        let uchar = fixture.keywords(&[BuiltinSet::UNSIGNED, BuiltinSet::CHAR]);
        let uchar_name = fixture.type_name(uchar, &[]);
        let max = constant(&mut fixture, 255, IntKind::Int);
        let over = constant(&mut fixture, 256, IntKind::Int);
        let held = [enumerator(&mut fixture, "held", Some(max))];
        let past = [enumerator(&mut fixture, "past", Some(over))];
        let overflowing =
            [enumerator(&mut fixture, "last", Some(max)), enumerator(&mut fixture, "after", None)];
        let good = enumeration(&mut fixture, Some("A"), Some(uchar_name), &held);
        let outside = enumeration(&mut fixture, Some("B"), Some(uchar_name), &past);
        let overflow = enumeration(&mut fixture, Some("C"), Some(uchar_name), &overflowing);
        let hole = fixture.declarator(None, &[]);
        let held_name = fixture.name("held");

        let mut checker = fixture.checker();
        let ty = checker.declared_type(good, hole);
        assert_eq!(underlying(&checker, ty), "unsigned char");
        // An enumerator of an enumeration whose representation the program wrote has that type
        // whether or not the value would have fitted in an `int`.
        assert_eq!(enumerator_binding(&checker, held_name), (255, "unsigned char".to_string()));
        assert!(messages(&checker).is_empty());

        checker.declared_type(outside, hole);
        checker.declared_type(overflow, hole);
        assert_eq!(
            messages(&checker),
            [
                "enumerator value outside the range of underlying type",
                "overflow in enumeration values",
            ]
        );
    }

    #[test]
    fn an_enum_with_nothing_in_it_and_an_enumerator_written_twice_are_each_reported() {
        let mut fixture = Fixture::new();
        let one = constant(&mut fixture, 1, IntKind::Int);
        let two = constant(&mut fixture, 2, IntKind::Int);
        let empty = enumeration(&mut fixture, Some("E"), None, &[]);
        let twice =
            [enumerator(&mut fixture, "a", Some(one)), enumerator(&mut fixture, "a", Some(two))];
        let repeated = enumeration(&mut fixture, Some("F"), None, &twice);
        let hole = fixture.declarator(None, &[]);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(empty, hole);
        assert_eq!(message(&checker), "empty enum is invalid");
        // Complete all the same, so that the declarations written against it are checked
        // against something rather than reported one by one.
        assert!(is_complete(&checker.types, ty));

        checker.declared_type(repeated, hole);
        assert_eq!(messages(&checker)[1], "redeclaration of enumerator 'a'");
    }

    #[test]
    fn an_enumerator_that_is_not_a_constant_is_reported_and_the_list_goes_on() {
        let mut fixture = Fixture::new();
        let name = fixture.name("n");
        let variable = fixture.ast.expr(ast::Expr::Name(name), Span::DUMMY);
        let enumerators =
            [enumerator(&mut fixture, "a", Some(variable)), enumerator(&mut fixture, "b", None)];
        let specs = enumeration(&mut fixture, Some("E"), None, &enumerators);
        let hole = fixture.declarator(None, &[]);
        let b = fixture.name("b");

        let mut checker = fixture.checker();
        let int = checker.int();
        checker.declare_object(name, int, Span::DUMMY);
        checker.declared_type(specs, hole);
        assert_eq!(message(&checker), "enumerator value for 'a' is not an integer constant");
        // The enumerator after it carries on from where the one that failed would have been.
        assert_eq!(enumerator_binding(&checker, b).0, 1);
    }
}
