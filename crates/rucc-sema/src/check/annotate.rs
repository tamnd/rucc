//! What a declaration, a member or a record was written with, kept for the program to ask about.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! Most attributes are read once, where they are written, and turned into whatever they change: a
//! section, a flag, an alignment in the layout. Two builtins need them again later.
//! `__builtin_has_attribute` asks whether a declaration or a type was written with one, and
//! `__builtin_counted_by_ref` asks which member a flexible array member's `counted_by` named. So
//! the lists are kept here, against the declaration, the member or the record they were written
//! on, and both builtins read them from here.
//!
//! The kernel is why either matters. `compiler.h` asks `__builtin_has_attribute(p, nonstring)`
//! of every argument to its string copying helpers once `__has_builtin` says the builtin is there,
//! and refuses to build when a buffer it knows is not a string is handed to one that wants one.
//! An answer of no for a member written `__nonstring` is a build that stops. `overflow.h` sets a
//! flexible array's counter through `__builtin_counted_by_ref` and takes the `void *` answer to
//! mean there is no counter to set.
//!
//! What is asked is answered the way gcc 15 answers it, measured one question at a time:
//!
//! * A declaration has what was written on any declaration of it, and then what its type has.
//! * A member has what was written on it and on its specifiers, and then what its type has.
//! * Any other expression has what its type has, so `p` pointing into a `nonstring` array has
//!   nothing and neither does `&a[0]`. Parentheses change nothing, since they are not in the tree.
//! * A type has the `aligned` and `may_alias` of each typedef on the way to it, the attributes
//!   written on its record, and `vector_size` when it is a vector.
//! * Arguments are compared when the question has them, so `aligned(16)` of something aligned to
//!   32 is no and `section(".bar")` of something in `.foo` is no. Without them the name is enough.
//! * An attribute gcc has never heard of is an error, and the answer is no.

use rucc_ast::{self as ast, AttrArg, AttrList, Attribute};
use rucc_base::Symbol;
use rucc_base::hash::Map;
use rucc_diag::{Diagnostic, Span};
use rucc_gnu::Kind;
use rucc_types::{ArrayLen, FieldDecl, RecordId, TypeId, TypeKind, is_array, is_integer, layout};

use crate::check::Checker;
use crate::check::attr::BIGGEST_ALIGNMENT;
use crate::decl::{DeclFlags, DeclId};
use crate::expr::{ExprId, ExprKind};
use crate::tast::Const;

/// The name the builtin that finds a flexible array's counter is called by.
const COUNTED_BY_REF: &str = "__builtin_counted_by_ref";

/// Whether a name is the one builtin here that is called rather than written as syntax.
#[cfg(test)]
pub(super) fn is_family(name: &str) -> bool {
    name == COUNTED_BY_REF
}

/// The attribute lists kept for the two builtins that ask about them.
#[derive(Debug, Default)]
pub(in crate::check) struct Annotations {
    /// Every list written on a declaration, from every declaration of it.
    decls: Map<DeclId, Vec<AttrList>>,
    /// The lists a member was written with, its own and its specifiers', by record and name.
    members: Map<(RecordId, Symbol), [AttrList; 2]>,
    /// The list written on a record's definition.
    records: Map<RecordId, AttrList>,
    /// The member a flexible array member's `counted_by` named, by record and array.
    counters: Map<(RecordId, Symbol), Symbol>,
}

/// One argument of an attribute, as something two of them can be compared by.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    /// A lone name, without any `__` armour.
    Name(String),
    /// An integer constant.
    Int(i128),
    /// A string literal, as its code units.
    Str(Vec<u32>),
}

/// The attributes gcc keeps as flags on a declaration rather than as a list, which a keyword
/// can set as well as an attribute. `_Noreturn` is the one that shows: gcc answers yes for
/// `noreturn` of a function declared with it and nothing else.
const FLAGGED: [(&str, DeclFlags); 6] = [
    ("noreturn", DeclFlags::NORETURN),
    ("weak", DeclFlags::WEAK),
    ("cold", DeclFlags::COLD),
    ("hot", DeclFlags::HOT),
    ("noinline", DeclFlags::NOINLINE),
    ("always_inline", DeclFlags::ALWAYS_INLINE),
];

impl Checker<'_> {
    /// Keeps the lists a declaration was written with, beside those of its earlier declarations.
    pub(in crate::check) fn annotate_decl(&mut self, decl: DeclId, lists: &[AttrList]) {
        let kept = self.annotations.decls.entry(decl).or_default();
        kept.extend(lists.iter().copied().filter(|list| !list.is_empty()));
    }

    /// Keeps the lists a member was written with.
    pub(in crate::check) fn annotate_member(&mut self, record: RecordId, field: ast::Field) {
        let Some(name) = self.member_name(field) else { return };
        let specs = self.ast[field.specs].attrs;
        self.annotations.members.insert((record, name), [field.attrs, specs]);
    }

    /// The name a member was declared with, which is on the declarator.
    fn member_name(&self, field: ast::Field) -> Option<Symbol> {
        let declarator = field.declarator?;
        self.ast[declarator].name
    }

    /// Keeps the list a record was defined with, and checks each `counted_by` among its members.
    ///
    /// Run once the members have been through the rule about where a flexible array member may
    /// sit, so a member still here with no length is one. The messages are gcc 15's.
    pub(in crate::check) fn annotate_record(
        &mut self,
        record: RecordId,
        attrs: AttrList,
        fields: &[(FieldDecl, Span)],
    ) {
        self.annotations.records.insert(record, attrs);
        for (at, (decl, _)) in fields.iter().enumerate() {
            let Some(name) = decl.name else { continue };
            let Some(lists) = self.annotations.members.get(&(record, name)).copied() else {
                continue;
            };
            let written = lists.iter().flat_map(|&list| self.ast[list].iter().copied());
            let asked: Vec<Attribute> =
                written.filter(|attr| self.is_named(attr, "counted_by")).collect();
            for attr in asked {
                if let Some(counter) = self.counter(attr, fields, at) {
                    self.annotations.counters.insert((record, name), counter);
                }
            }
        }
    }

    /// The member one `counted_by` names, once it has been checked to be one it may name.
    fn counter(
        &mut self,
        attr: Attribute,
        fields: &[(FieldDecl, Span)],
        at: usize,
    ) -> Option<Symbol> {
        let (decl, _) = fields[at];
        let refuse = |checker: &mut Self, what: String| {
            checker.report(Diagnostic::error(what, attr.span).with_code("E0765"));
            None
        };
        let canonical = self.types.canonical(decl.ty);
        if !is_array(&self.types, canonical) {
            return refuse(
                self,
                "'counted_by' attribute is not allowed for a non-array field".into(),
            );
        }
        let flexible =
            matches!(self.types.kind(canonical), TypeKind::Array { len: ArrayLen::Unknown, .. });
        if !flexible || at + 1 != fields.len() {
            let what =
                "'counted_by' attribute is not allowed for a non-flexible array member field";
            return refuse(self, what.into());
        }
        let args = self.ast[attr.args].to_vec();
        let [AttrArg::Ident(counter)] = args.as_slice() else {
            return refuse(self, "'counted_by' argument is not an identifier".into());
        };
        let counter = *counter;
        let (spelled, array) = (self.text(counter).to_owned(), self.text(decl.name?).to_owned());
        let Some((found, _)) = fields.iter().find(|(field, _)| field.name == Some(counter)) else {
            let what = format!(
                "argument '{spelled}' to the 'counted_by' attribute is not a field declaration in \
                 the same structure as '{array}'"
            );
            return refuse(self, what);
        };
        if !is_integer(&self.types, self.types.canonical(found.ty)) {
            let what = format!(
                "argument '{spelled}' to the 'counted_by' attribute is not a field declaration \
                 with an integer type"
            );
            return refuse(self, what);
        }
        Some(counter)
    }

    /// Whether an attribute is the one named, armour and the `gnu` namespace allowed.
    fn is_named(&self, attr: &Attribute, name: &str) -> bool {
        if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
            return false;
        }
        rucc_gnu::unarmour(self.text(attr.name)) == name
    }

    /// `__builtin_has_attribute(expr, attribute)`.
    pub(in crate::check) fn has_attribute_expr(
        &mut self,
        operand: ast::ExprId,
        attr: AttrList,
        span: Span,
    ) -> ExprId {
        let operand = self.expr(operand);
        if self.is_poisoned(operand) {
            return self.poison(span);
        }
        let Some(asked) = self.asked(attr) else { return self.answer(false, span) };
        let ty = self.tast[operand].ty;
        let found = match self.tast[operand].kind {
            ExprKind::Decl(decl) => self.decl_has(decl, asked),
            ExprKind::Member { base, field } => self.member_has(base, field, asked),
            _ => false,
        };
        let found = found || self.type_has(ty, asked);
        self.answer(found, span)
    }

    /// `__builtin_has_attribute(type, attribute)`.
    pub(in crate::check) fn has_attribute_type(
        &mut self,
        ty: ast::TypeNameId,
        attr: AttrList,
        span: Span,
    ) -> ExprId {
        let ty = self.type_name(ty);
        let Some(asked) = self.asked(attr) else { return self.answer(false, span) };
        let found = self.type_has(ty, asked);
        self.answer(found, span)
    }

    /// The attribute a question asks about, or nothing when gcc has never heard of it.
    fn asked(&mut self, attr: AttrList) -> Option<Attribute> {
        let asked = *self.ast[attr].first()?;
        let name = rucc_gnu::unarmour(self.text(asked.name));
        if rucc_gnu::lookup(Kind::Attribute, name).is_none() {
            let what = format!("unknown attribute '{name}'");
            self.report(Diagnostic::error(what, asked.span).with_code("E0766"));
            return None;
        }
        Some(asked)
    }

    /// The answer, as the `int` constant gcc gives.
    fn answer(&mut self, found: bool, span: Span) -> ExprId {
        let int = self.int();
        self.constant(Const::Int(i128::from(found)), int, span)
    }

    /// Whether any declaration of a name was written with the attribute.
    fn decl_has(&mut self, decl: DeclId, asked: Attribute) -> bool {
        if asked.args.is_empty() {
            let name = rucc_gnu::unarmour(self.text(asked.name));
            let flags = self.tast[decl].flags;
            let flagged = FLAGGED.iter().find(|(flagged, _)| *flagged == name);
            if flagged.is_some_and(|&(_, flag)| flags.contains(flag)) {
                return true;
            }
        }
        let lists = self.annotations.decls.get(&decl).cloned().unwrap_or_default();
        self.any_has(&lists, asked)
    }

    /// Whether the member an access reaches was written with the attribute.
    fn member_has(&mut self, base: ExprId, field: u32, asked: Attribute) -> bool {
        let base = self.types.canonical(self.tast[base].ty);
        let TypeKind::Record(record) = self.types.kind(base) else { return false };
        let Some(name) = self.types.record_info(record).fields[field as usize].name else {
            return false;
        };
        let Some(lists) = self.annotations.members.get(&(record, name)).copied() else {
            return false;
        };
        self.any_has(&lists, asked)
    }

    /// Whether a type, or a typedef on the way to it, has the attribute.
    fn type_has(&mut self, ty: TypeId, asked: Attribute) -> bool {
        let name = rucc_gnu::unarmour(self.text(asked.name)).to_owned();
        let wanted = self.values(asked);
        let mut ty = ty;
        loop {
            match self.types.kind(ty) {
                TypeKind::Typedef { underlying, align, may_alias, .. } => {
                    let aligned = align.map(|align| Value::Int(i128::from(align.get())));
                    if name == "aligned"
                        && aligned.is_some()
                        && wanted.iter().all(|w| Some(w) == aligned.as_ref())
                    {
                        return true;
                    }
                    if name == "may_alias" && may_alias {
                        return true;
                    }
                    ty = underlying;
                }
                TypeKind::Record(record) => {
                    let Some(&list) = self.annotations.records.get(&record) else { return false };
                    return self.any_has(&[list], asked);
                }
                TypeKind::Vector { .. } if name == "vector_size" => {
                    let size = layout(&self.types, ty, self.cx.target).ok().map(|at| at.size);
                    let size = size.map(|size| Value::Int(i128::from(size)));
                    return wanted.iter().all(|w| Some(w) == size.as_ref());
                }
                _ => return false,
            }
        }
    }

    /// Whether any attribute of the lists is the one asked about, arguments and all.
    fn any_has(&mut self, lists: &[AttrList], asked: Attribute) -> bool {
        let name = rucc_gnu::unarmour(self.text(asked.name)).to_owned();
        let wanted = self.values(asked);
        for &list in lists {
            let written = self.ast[list].to_vec();
            for attr in written {
                if !self.is_named(&attr, &name) {
                    continue;
                }
                if wanted.is_empty() || self.written_values(attr) == wanted {
                    return true;
                }
            }
        }
        false
    }

    /// The arguments a question was asked with, where none means any.
    fn values(&mut self, attr: Attribute) -> Vec<Value> {
        let args = self.ast[attr.args].to_vec();
        args.into_iter().map(|arg| self.value_of(arg)).collect()
    }

    /// The arguments an attribute was written with, with `aligned` written bare given the
    /// alignment it stands for so that it compares with one written out.
    fn written_values(&mut self, attr: Attribute) -> Vec<Value> {
        if attr.args.is_empty() && self.is_named(&attr, "aligned") {
            return vec![Value::Int(i128::from(BIGGEST_ALIGNMENT))];
        }
        self.values(attr)
    }

    /// One argument as a value. Something that is neither a name nor a constant compares
    /// unequal to everything, itself included, which is the answer gcc gives for it.
    fn value_of(&mut self, arg: AttrArg) -> Value {
        let expr = match arg {
            AttrArg::Ident(name) => return Value::Name(rucc_gnu::unarmour(self.text(name)).into()),
            AttrArg::Expr(expr) => self.expr(expr),
        };
        if let ExprKind::Str(id) = self.tast[expr].kind {
            return Value::Str(self.tast[id].elements.clone());
        }
        match self.eval_integer(expr) {
            Ok(value) => Value::Int(value),
            Err(_) => Value::Name(String::new()),
        }
    }

    /// `__builtin_counted_by_ref(array)`, if that is the name called.
    ///
    /// A pointer to the member a flexible array member's `counted_by` names, reached through the
    /// same object the array was, or a null `void *` for an array with no counter. Checked before
    /// the argument is turned into a value, since an array that has decayed is a pointer and the
    /// argument has to be the array.
    pub(in crate::check) fn counted_by_ref_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        if self.text(name) != COUNTED_BY_REF {
            return None;
        }
        let args = self.ast[args].to_vec();
        let [arg] = args.as_slice() else {
            let what = format!("wrong number of arguments to '{COUNTED_BY_REF}'");
            self.report(Diagnostic::error(what, span).with_code("E0767"));
            return Some(self.poison(span));
        };
        let operand = self.expr(*arg);
        if self.is_poisoned(operand) {
            return Some(self.poison(span));
        }
        let ty = self.types.canonical(self.tast[operand].ty);
        if !is_array(&self.types, ty) {
            let what = format!("the argument to '{COUNTED_BY_REF}' must be an array");
            self.report(Diagnostic::error(what, span).with_code("E0767"));
            return Some(self.poison(span));
        }
        if let Some((base, index)) = self.counter_of(operand) {
            let member = self.member_node(base, index, span);
            return Some(self.address_of(member, span));
        }
        let zero = self.answer(false, span);
        let void = self.types.void();
        let void = self.types.pointer(void);
        Some(self.conv().to_type(zero, void))
    }

    /// The object and the index of the member that counts an array, when it has one.
    fn counter_of(&self, array: ExprId) -> Option<(ExprId, u32)> {
        let ExprKind::Member { base, field } = self.tast[array].kind else { return None };
        let TypeKind::Record(record) = self.types.kind(self.types.canonical(self.tast[base].ty))
        else {
            return None;
        };
        let fields = &self.types.record_info(record).fields;
        let name = fields[field as usize].name?;
        let counter = *self.annotations.counters.get(&(record, name))?;
        let index = fields.iter().position(|field| field.name == Some(counter))?;
        Some((base, u32::try_from(index).ok()?))
    }
}
