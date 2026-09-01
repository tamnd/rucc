//! Turning what a declaration wrote into a type: the specifiers, then the declarator.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.1.
//!
//! A C declaration says its type in two halves that are read in opposite directions. The
//! specifiers are a set and are read together, so `unsigned static const long int` is a
//! perfectly ordinary `static const unsigned long`. The declarator is a sequence and is read
//! outward from the name, so `int (*f[3])(char)` says that `f` is an array of three pointers to
//! functions taking `char` and returning `int`. This is the one place that knows both, and
//! everything downstream asks the type table rather than reading a specifier list again.
//!
//! # Two directions, one fold
//!
//! [`Derived`](rucc_ast::Derived) is already in the spoken order, nearest the name first, which
//! the parser produced by pushing while it descended into the parentheses. So the type is built
//! by folding that list from its far end onto the type the specifiers named, and the step
//! nearest the name is applied last. That is why the loop below runs backwards and why the
//! index being zero is worth a name: an array is only allowed to say `static` or carry
//! qualifiers when it is the step nearest the name of a parameter, which is what makes
//! `void f(int a[const 3])` legal and `int a[const 3];` not.
//!
//! # What is not here yet
//!
//! The attributes a member carries. `_Alignas` on a member, `packed` on a member or on the
//! record, and the `#pragma pack` around it are each a number
//! [`FieldDecl`](rucc_types::FieldDecl) already has a place for, and none of them is filled in,
//! because what fills them in is attribute checking rather than type building.
//!
//! `auto` as a type specifier needs the initializer that it takes its type from, so it waits on
//! initialization. `_Complex` on an integer type, which gcc accepts, has no type to build
//! because [`TypeKind::Complex`](rucc_types::TypeKind::Complex) holds a floating kind, and
//! `_Imaginary` is a keyword gcc has never implemented either.

use std::collections::{HashMap, HashSet};

use rucc_ast::{
    self as ast, ArraySize, Complexity, Derived, ParamKind, Scalar, TypeSpec, TypeofArg,
};
use rucc_base::float::Format;
use rucc_base::{Idx, Symbol, sym};
use rucc_diag::{Diagnostic, Span};
use rucc_session::Std;
use rucc_target::{TargetInfo, VaList};
use rucc_types::{
    ArrayLen, FieldDecl, FloatKind, FunctionType, IntKind, Qualifiers, RecordKind, RecordOptions,
    TypeId, adjust_parameter, is_complete, is_function, is_integer, is_pointer, is_void, layout,
};

use crate::check::Checker;
use crate::decl::DeclId;
use crate::scope::{Binding, Tag, TagKind};

mod tag;

/// The widest `_BitInt` there is, which is the width the constant folding can hold.
///
/// This is not gcc's number. gcc 16 has `__BITINT_MAXWIDTH__` at sixty five thousand five
/// hundred and thirty five, which its own arbitrary precision arithmetic can fold and this
/// compiler's hundred and twenty eight bit constants cannot. Widening it is a change to
/// [`Const`](crate::Const) rather than to this, and until that happens the limit is reported
/// where a wider one is written rather than silently truncated. `__BITINT_MAXWIDTH__` in
/// `rucc-pp` is this same number and has to be changed with it.
const MAX_BIT_INT_WIDTH: u32 = 128;

/// The largest object, which is what an array size is measured against.
///
/// gcc prints this number in the message it produces, so it is written the way the message
/// needs it rather than as a target property. Every target this compiler has is 64-bit.
const MAX_OBJECT_SIZE: u64 = i64::MAX as u64;

/// What the type builder has worked out already and is not going to work out twice.
#[derive(Debug, Default)]
pub(crate) struct Built {
    /// What each specifier list turned out to name.
    ///
    /// The declarators of one declaration share one specifier list, and `struct { int x; } a, b;`
    /// declares one structure and not two, so building the type a second time is not a slow way
    /// to get the same answer, it is a different answer. Every specifier list is written at one
    /// place in the source and is checked once, so remembering what it named is enough.
    specified: HashMap<ast::DeclSpecsId, TypeId>,
    /// The tag types whose body has been read.
    ///
    /// This is what tells a redefinition from a completion, and the type table cannot answer it
    /// on its own. A record is complete exactly when its body has been read, but C23's
    /// `enum E : int;` is a complete type that has never had one, so `enum E : int;` followed by
    /// `enum E : int { A };` is a definition of something already complete and is allowed.
    defined: HashSet<TypeId>,
    /// What the named parameters of each prototype were declared as, by the first parameter of
    /// the list.
    ///
    /// A function definition binds these again in the body's scope, so that the `n` a prototype
    /// saw in `void f(int n, int a[n])` and the `n` the body assigns to are one declaration
    /// rather than two that happen to share a name. The key is the first parameter because a run
    /// of indices is not something a map can be keyed by, and a prototype always has at least one
    /// parameter in it: `(void)` and `()` are parameter lists of other kinds.
    params: HashMap<Idx<ast::Param>, Vec<DeclId>>,
    /// The target's `__builtin_va_list`, once something has asked for it.
    ///
    /// Every mention of the keyword names one type, so this is not only a cache. On the targets
    /// where the type is a record, building it a second time would build a second record, and
    /// two records with the same members are still two types, so `va_list a, b; a = b;` would
    /// stop being an assignment and start being a diagnostic.
    va_list: Option<TypeId>,
}

/// Who a declaration is about, for the diagnostics that name it.
///
/// gcc writes every one of these two ways, once for a declarator with a name and once for the
/// abstract declarator of a type name: `declaration of 'a' as array of voids` against
/// `declaration of type name as array of voids`. The two are kept together here so that a
/// message cannot be written in only one of its forms.
#[derive(Debug, Clone, Copy)]
struct Subject {
    /// The name, absent in an abstract declarator.
    name: Option<Symbol>,
    /// What a diagnostic about the declarator points at.
    span: Span,
}

/// What one mention of a tag turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagUse {
    /// The tag names a type already, which is that type.
    Known(TypeId),
    /// The tag names nothing yet, so this mention is what declares it.
    New,
    /// There was no tag, so there is nothing to bind and a fresh type every time.
    Anonymous,
    /// The tag names something of another kind, which has been reported.
    Wrong,
}

/// Where a declarator is being read, which decides what it is allowed to say.
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::check) struct Place {
    /// Whether this declarator is a function parameter, which is what lets its outermost array
    /// carry `static` and qualifiers, since those belong to the pointer it becomes.
    parameter: bool,
    /// Whether this declarator is a member of a structure or a union, which is what names the
    /// place in the one message that has to name it.
    member: bool,
    /// Whether this declarator is inside a function prototype, which is the only place `[*]`
    /// means anything.
    prototype: bool,
}

/// A member of a structure or a union, which is the one place that is named by a constant.
pub(in crate::check) const MEMBER: Place =
    Place { parameter: false, member: true, prototype: false };

impl Checker<'_> {
    /// The type a type name names, which is a cast, a `sizeof` or a `_Generic` association.
    pub fn type_name(&mut self, id: ast::TypeNameId) -> TypeId {
        let name = self.ast[id];
        self.declared_type(name.specs, name.declarator)
    }

    /// The type one declarator of one declaration declares.
    ///
    /// Always gives back a type. A declarator that does not check is reported and answered with
    /// the nearest thing that does, so that the declaration around it is still checked instead
    /// of collapsing, which is the same rule the expressions follow with their poisoned nodes.
    pub fn declared_type(
        &mut self,
        specs: ast::DeclSpecsId,
        declarator: ast::DeclaratorId,
    ) -> TypeId {
        self.build_type(specs, declarator, Place::default())
    }

    /// The type a declaration with no declarator names, which is what declares a tag.
    ///
    /// `struct S { int x; };` builds and lays out the record even though there is nothing for it
    /// to be the type of, since that is where its members are checked.
    pub(in crate::check) fn declared_specs(&mut self, specs: ast::DeclSpecsId) -> TypeId {
        let span = self.ast[specs].span;
        self.specified_type(specs, Subject { name: None, span }, Place::default())
    }

    /// Declares a typedef name in the current scope.
    ///
    /// Public for the same reason [`Checker::declare_object`] is: a caller that checks one
    /// expression rather than a translation unit still needs a way to say that a name the
    /// parser already decided was a type is one.
    pub fn declare_typedef(&mut self, name: Symbol, ty: TypeId) {
        self.scopes.declare(name, Binding::Typedef(ty));
    }

    /// The specifiers and the declarator, in the place they were written.
    fn build_type(
        &mut self,
        specs: ast::DeclSpecsId,
        declarator: ast::DeclaratorId,
        place: Place,
    ) -> TypeId {
        let node = self.ast[declarator];
        let subject = Subject {
            name: node.name,
            span: if node.name.is_some() { node.name_span } else { node.span },
        };
        let base = self.specified_type(specs, subject, place);
        self.derive(base, declarator, subject, place)
    }

    /// The type the specifiers alone name, qualifiers included.
    ///
    /// Answered once per specifier list rather than once per declarator, because the
    /// declarators of one declaration share one list and `struct { int x; } a, b;` declares one
    /// structure. Building it twice would not be a slow way to the same answer, it would be two
    /// structures and a second warning about anything the specifiers themselves got wrong.
    fn specified_type(&mut self, id: ast::DeclSpecsId, subject: Subject, place: Place) -> TypeId {
        if let Some(&ty) = self.built.specified.get(&id) {
            return ty;
        }
        let specs = self.ast[id];
        let base = self.type_spec(specs.ty, specs.span, subject, place);
        let ty = self.qualify(base, specs.quals, specs.span);
        self.built.specified.insert(id, ty);
        ty
    }

    /// The type a single type specifier names.
    fn type_spec(&mut self, spec: TypeSpec, span: Span, subject: Subject, place: Place) -> TypeId {
        match spec {
            TypeSpec::None => {
                let what = match subject.name {
                    Some(name) => format!("in declaration of '{}'", self.text(name)),
                    None => String::new(),
                };
                let message = format!("type defaults to 'int' {what}");
                self.report(
                    Diagnostic::error(message.trim_end().to_string(), subject.span)
                        .with_code("E0526"),
                );
                self.int()
            }
            TypeSpec::Builtin(builtin) => match builtin.resolve() {
                Some(basic) => self.basic_type(basic.scalar, basic.complexity, span),
                None => {
                    self.report(
                        Diagnostic::error(
                            "two or more data types in declaration specifiers".to_string(),
                            span,
                        )
                        .with_code("E0525"),
                    );
                    self.int()
                }
            },
            TypeSpec::Record { kind, tag, fields, .. } => self.record_spec(kind, tag, fields, span),
            TypeSpec::Enum { tag, enumerators, underlying, .. } => {
                self.enum_spec(tag, enumerators, underlying, span)
            }
            TypeSpec::Typedef(name) => match self.scopes.lookup(name) {
                Some(Binding::Typedef(ty)) => ty,
                // The parser only writes this down for a name its own scope stack said was a
                // type, so the two disagreeing means a declaration was checked in one and not
                // in the other. Reported rather than assumed, since the alternative is a
                // declaration that silently means something else.
                _ => {
                    let name = self.text(name).to_owned();
                    self.report(
                        Diagnostic::error(format!("unknown type name '{name}'"), span)
                            .with_code("E0546"),
                    );
                    self.int()
                }
            },
            TypeSpec::Typeof { unqual, operand } => self.typeof_type(unqual, operand),
            TypeSpec::Atomic(inner) => {
                let inner = self.type_name(inner);
                self.atomic_type(inner, span)
            }
            TypeSpec::VaList => self.va_list_type(),
            TypeSpec::Auto(which) => {
                // The deduction itself is in `check/decl.rs`, which takes the declaration apart
                // before it asks for a type at all. What reaches here is the specifier written
                // where nothing deduces anything, and the two places that can happen are a
                // member and a parameter: a type name cannot be written with it, and every
                // other declaration goes through the deduction. gcc turns both away in its
                // parser and says so in terms of its grammar. clang says what is wrong with the
                // declaration instead, and that is the wording used here.
                let spelled = which.spelling();
                let place = if place.member { "struct member" } else { "function prototype" };
                self.report(
                    Diagnostic::error(format!("'{spelled}' not allowed in {place}"), span)
                        .with_code("E0651"),
                );
                self.int()
            }
        }
    }

    /// One of the types a keyword or a run of keywords names.
    fn basic_type(&mut self, scalar: Scalar, complexity: Complexity, span: Span) -> TypeId {
        let kind = int_kind(scalar);
        let float = float_kind(scalar, self.cx.target);

        match complexity {
            Complexity::Real => match (scalar, kind, float) {
                (Scalar::Void, _, _) => self.types.void(),
                (Scalar::Bool, _, _) => self.types.boolean(),
                (Scalar::BitInt { width, unsigned }, _, _) => self.bit_int_type(width, !unsigned),
                (_, Some(kind), _) => self.types.int(kind),
                (_, _, Some(kind)) => self.types.float(kind),
                // A floating type the target does not have. `_Float128x` is one no target gcc
                // supports has, and `__float80` is one only x86 has, and gcc turns both of them
                // away in the same words.
                (Scalar::Float128x | Scalar::Float80, _, _) => {
                    self.unavailable_type(spell_scalar(scalar), span);
                    self.types.float(FloatKind::Double)
                }
                // The decimal floating types are named by keywords the lexer and the parser
                // both know and are deferred past 1.0 by `spec/19-open-questions.md`, so one of
                // them is refused where it is written rather than given a type it is not.
                _ => {
                    self.unsupported_type(&format!("the type `{}`", spell_scalar(scalar)), span);
                    self.types.float(FloatKind::Double)
                }
            },
            Complexity::Complex => match float {
                Some(kind) => self.types.complex(kind),
                // gcc accepts `_Complex int`. There is no type for it here, since a complex
                // type holds a floating kind, and inventing one for a GNU extension nothing
                // uses is not worth what it costs every reader of that enum.
                None => {
                    let what = format!("`_Complex` on the type `{}`", spell_scalar(scalar));
                    self.unsupported_type(&what, span);
                    self.types.complex(FloatKind::Double)
                }
            },
            // gcc parses this keyword and has never implemented the type behind it.
            Complexity::Imaginary => {
                self.unsupported_type("`_Imaginary`", span);
                self.types.complex(float.unwrap_or(FloatKind::Double))
            }
        }
    }

    /// `__builtin_va_list`, which is the one type the target names rather than the source.
    ///
    /// Built once and remembered, since every mention of the keyword is the same type and the
    /// record ones would otherwise be a new record each time. The members are the psABI's, named
    /// as the psABI names them: nothing here reads them, and a program that prints a `va_list`
    /// in a debugger reads all of them.
    pub(crate) fn va_list_type(&mut self) -> TypeId {
        if let Some(ty) = self.built.va_list {
            return ty;
        }
        let ty = match self.cx.target.va_list {
            VaList::CharPointer => {
                let elem = self.types.int(IntKind::Char);
                self.types.pointer(elem)
            }
            VaList::VoidPointer => {
                let elem = self.types.void();
                self.types.pointer(elem)
            }
            VaList::SysV => {
                let uint = self.types.int(IntKind::UInt);
                let void = self.types.void();
                let ptr = self.types.pointer(void);
                let record = self.builtin_record(
                    sym::VA_LIST_TAG,
                    &[
                        (sym::GP_OFFSET, uint),
                        (sym::FP_OFFSET, uint),
                        (sym::OVERFLOW_ARG_AREA, ptr),
                        (sym::REG_SAVE_AREA, ptr),
                    ],
                );
                // The array of one is the whole reason a SysV `va_list` can be handed to
                // `vfprintf` and come back moved on: what is passed is the address of the one
                // element, so the callee reads and writes the caller's list rather than a copy.
                self.types.array(record, ArrayLen::Fixed(1))
            }
            VaList::Aapcs => {
                let int = self.types.int(IntKind::Int);
                let void = self.types.void();
                let ptr = self.types.pointer(void);
                self.builtin_record(
                    sym::VA_LIST,
                    &[
                        (sym::STACK, ptr),
                        (sym::GR_TOP, ptr),
                        (sym::VR_TOP, ptr),
                        (sym::GR_OFFS, int),
                        (sym::VR_OFFS, int),
                    ],
                )
            }
        };
        self.built.va_list = Some(ty);
        ty
    }

    /// A complete record the compiler builds itself, with a tag and ordinary members.
    ///
    /// Not put in the tag scope. A program that writes `struct __va_list_tag` is writing about a
    /// tag of its own, which is what gcc's own headers do not do and what the one program that
    /// does would have meant either way.
    fn builtin_record(&mut self, tag: Symbol, members: &[(Symbol, TypeId)]) -> TypeId {
        let id = self.types.declare_record(RecordKind::Struct, Some(tag));
        let decls: Vec<FieldDecl> =
            members.iter().map(|&(name, ty)| FieldDecl::new(Some(name), ty)).collect();
        let laid_out = rucc_types::layout_record(
            &self.types,
            RecordKind::Struct,
            &decls,
            &RecordOptions::default(),
            self.cx.target,
        )
        .expect("a record of pointers and integers lays out");
        self.types.complete_record(id, laid_out);
        self.types.record(id)
    }

    /// `typeof(x)` or `typeof(T)`, and the C23 spelling that takes the qualifiers off.
    fn typeof_type(&mut self, unqual: bool, operand: TypeofArg) -> TypeId {
        // The expression is checked and never evaluated, 6.7.2.5p3. Checking it is what gives
        // it a type at all, and an array or a function keeps its own type here, since nothing
        // has asked for its value and it is the asking that decays it.
        let ty = match operand {
            TypeofArg::Expr(expr) => {
                let node = self.expr(expr);
                self.tast[node].ty
            }
            TypeofArg::Type(name) => self.type_name(name),
        };
        if !unqual {
            return ty;
        }
        // `typeof_unqual` takes off the qualifiers and `_Atomic` with them, which is the one
        // place the two spellings of `_Atomic` are told apart again.
        let bare = match self.types.kind(self.types.canonical(ty)) {
            rucc_types::TypeKind::Atomic(inner) => inner,
            _ => ty,
        };
        self.types.unqualified(bare)
    }

    /// `_BitInt(N)`, whose width is a constant expression and has a range.
    ///
    /// A signed one is never narrower than two bits, because one of them is the sign and a type
    /// with no value bits is not a type. An unsigned one may be a single bit, and
    /// `unsigned _BitInt(1)` holding nothing but zero and one is a legal, if peculiar, type.
    fn bit_int_type(&mut self, width: ast::ExprId, signed: bool) -> TypeId {
        let value = self.expr(width);
        let span = self.tast.expr_span(value);
        let Ok(bits) = self.eval_integer(value) else {
            // Poisoned or not a constant. The second case is worth its own sentence, since
            // `_BitInt(n)` reads as if it might be a variably modified type and is not.
            if !self.is_poisoned(value) {
                self.report(
                    Diagnostic::error(
                        "'_BitInt' argument is not an integer constant expression".to_string(),
                        span,
                    )
                    .with_code("E0529"),
                );
            }
            return self.int();
        };
        if bits <= 0 {
            let message = format!(
                "'_BitInt' argument '{bits}' is not a positive integer constant expression"
            );
            self.report(Diagnostic::error(message, span).with_code("E0529"));
            return self.int();
        }
        if signed && bits < 2 {
            let message = "'signed _BitInt' argument must be at least 2".to_string();
            self.report(Diagnostic::error(message, span).with_code("E0529"));
            return self.int();
        }
        if bits > i128::from(MAX_BIT_INT_WIDTH) {
            let message = format!(
                "'_BitInt' argument '{bits}' is larger than 'BITINT_MAXWIDTH' '{MAX_BIT_INT_WIDTH}'"
            );
            self.report(Diagnostic::error(message, span).with_code("E0529"));
            return self.int();
        }
        let bits = u32::try_from(bits).unwrap_or(MAX_BIT_INT_WIDTH);
        self.types.bit_int(signed, bits)
    }

    /// `_Atomic(T)`, which is a type and not a qualifier and which two things cannot be.
    fn atomic_type(&mut self, inner: TypeId, span: Span) -> TypeId {
        let canonical = self.types.canonical(inner);
        let what = if rucc_types::is_array(&self.types, canonical) {
            "'_Atomic'-qualified array type"
        } else if is_function(&self.types, canonical) {
            "'_Atomic'-qualified function type"
        } else if !self.types.quals(inner).is_none() {
            "'_Atomic' applied to a qualified type"
        } else {
            return self.types.atomic(inner);
        };
        self.report(Diagnostic::error(what.to_string(), span).with_code("E0527"));
        inner
    }

    /// A `struct` or a `union`, referred to by tag or declared by one.
    fn record_spec(
        &mut self,
        kind: ast::RecordKind,
        tag: Option<Symbol>,
        fields: Option<ast::MemberList>,
        span: Span,
    ) -> TypeId {
        let (kind, tag_kind) = match kind {
            ast::RecordKind::Struct => (RecordKind::Struct, TagKind::Struct),
            ast::RecordKind::Union => (RecordKind::Union, TagKind::Union),
        };
        let Some(members) = fields else {
            return match self.tag_use(tag, tag_kind, span) {
                TagUse::Known(ty) => ty,
                found => {
                    let id = self.types.declare_record(kind, tag);
                    let ty = self.types.record(id);
                    self.bind_tag(found, tag, tag_kind, ty);
                    ty
                }
            };
        };
        // The tag is bound before the members are read, which is what makes
        // `struct S { struct S *next; };` refer to the structure being defined rather than
        // declare a second one inside it.
        let (id, ty) = self.record_defined(kind, tag, tag_kind, span);
        self.built.defined.insert(ty);
        self.record_body(id, kind, members, span);
        ty
    }

    /// An `enum`, referred to by tag or declared by one, with C23's underlying type.
    fn enum_spec(
        &mut self,
        tag: Option<Symbol>,
        enumerators: Option<ast::EnumeratorList>,
        underlying: Option<ast::TypeNameId>,
        span: Span,
    ) -> TypeId {
        let underlying = underlying.map(|name| {
            let ty = self.type_name(name);
            if is_integer(&self.types, self.types.canonical(ty)) {
                return ty;
            }
            self.report(
                Diagnostic::error("invalid 'enum' underlying type".to_string(), span)
                    .with_code("E0530"),
            );
            self.int()
        });

        let Some(list) = enumerators else {
            return match self.tag_use(tag, TagKind::Enum, span) {
                TagUse::Known(ty) => ty,
                found => {
                    let id = self.types.declare_enum(tag);
                    // An enumeration whose representation the program wrote is complete from the
                    // point it says so, which is the whole reason C23 lets it be written:
                    // `enum E : int;` is a forward declaration usable by value straight away.
                    if let Some(underlying) = underlying {
                        self.types.complete_enum(id, underlying, true);
                    }
                    let ty = self.types.enumeration(id);
                    self.bind_tag(found, tag, TagKind::Enum, ty);
                    ty
                }
            };
        };
        let (id, ty) = self.enum_defined(tag, span);
        self.built.defined.insert(ty);
        self.enum_body(id, list, underlying, span);
        ty
    }

    /// What a mention of a tag turns out to be.
    ///
    /// Which scope a tag goes in is not decided here. `struct S;` on its own declares a new type
    /// in the current scope even where an outer one is visible, and `struct S *p;` refers to
    /// whatever `S` already means, and the difference is whether the declaration had any
    /// declarators, which is a fact about the declaration and not about the type. So this asks
    /// the whole stack, and the declaration checking that knows the difference will ask
    /// [`Scopes::tag_here`](crate::Scopes::tag_here) before it gets here.
    fn tag_use(&mut self, tag: Option<Symbol>, kind: TagKind, span: Span) -> TagUse {
        // No tag is nothing to look up and nothing to bind: the type is reachable only through
        // the declarators of the one declaration that wrote it.
        let Some(name) = tag else { return TagUse::Anonymous };
        match self.scopes.tag(name) {
            Some(found) if found.kind == kind => TagUse::Known(found.ty),
            Some(_) => {
                let spelled = self.text(name).to_owned();
                self.report(
                    Diagnostic::error(format!("'{spelled}' defined as wrong kind of tag"), span)
                        .with_code("E0531"),
                );
                TagUse::Wrong
            }
            None => TagUse::New,
        }
    }

    /// Binds a tag to the type just built for it, where this mention is what declares it.
    fn bind_tag(&mut self, found: TagUse, tag: Option<Symbol>, kind: TagKind, ty: TypeId) {
        // A tag that already means something else keeps meaning it. Rebinding would turn one
        // diagnostic into one per use, and the uses that follow were written against the
        // declaration that is already there.
        if !matches!(found, TagUse::New) {
            return;
        }
        if let Some(name) = tag {
            self.scopes.declare_tag(name, Tag { kind, ty });
        }
    }

    /// The qualifiers a specifier list or a pointer wrote, applied to the type they qualify.
    pub(in crate::check) fn qualify(
        &mut self,
        ty: TypeId,
        quals: ast::Quals,
        span: Span,
    ) -> TypeId {
        // `_Atomic` written where a qualifier goes qualifies whatever the declarator arrives at,
        // and it constructs a type rather than adding a bit to one, which is why it is applied
        // before the qualifiers rather than beside them.
        let ty = if quals.has(ast::Quals::ATOMIC) { self.atomic_type(ty, span) } else { ty };
        let mut result = Qualifiers::NONE;
        if quals.has(ast::Quals::CONST) {
            result = result.with(Qualifiers::CONST);
        }
        if quals.has(ast::Quals::VOLATILE) {
            result = result.with(Qualifiers::VOLATILE);
        }
        if quals.has(ast::Quals::RESTRICT) {
            if is_pointer(&self.types, self.types.canonical(ty)) {
                result = result.with(Qualifiers::RESTRICT);
            } else {
                self.report(
                    Diagnostic::error("invalid use of 'restrict'".to_string(), span)
                        .with_code("E0528"),
                );
            }
        }
        self.types.qualified(ty, result)
    }

    /// Folds the declarator onto the type the specifiers named.
    fn derive(
        &mut self,
        base: TypeId,
        declarator: ast::DeclaratorId,
        subject: Subject,
        place: Place,
    ) -> TypeId {
        // The tree outlives the checker's own borrows, so taking the reference out first is
        // what lets the loop below call methods that take the checker mutably.
        let ast = self.ast;
        let steps = &ast[ast[declarator].derived];
        let mut ty = base;
        for (index, step) in steps.iter().enumerate().rev() {
            // Nearest the name, which is the step a parameter's adjustment applies to and the
            // only one allowed to write `static` or a qualifier inside its brackets.
            let nearest = index == 0;
            ty = match *step {
                Derived::Pointer { quals, .. } => {
                    let pointer = self.types.pointer(ty);
                    self.qualify(pointer, quals, subject.span)
                }
                Derived::Array { size, quals, has_static } => {
                    if (!quals.is_none() || has_static) && !(place.parameter && nearest) {
                        self.report(
                            Diagnostic::error(
                                "static or type qualifiers in non-parameter array declarator"
                                    .to_string(),
                                subject.span,
                            )
                            .with_code("E0540"),
                        );
                    }
                    self.array_of(ty, size, subject, place)
                }
                Derived::Function { params, variadic, kind } => {
                    self.function_of(ty, params, variadic, kind, subject)
                }
            };
        }
        ty
    }

    /// An array of the element type the steps closer to the name arrived at.
    fn array_of(
        &mut self,
        elem: TypeId,
        size: ArraySize,
        subject: Subject,
        place: Place,
    ) -> TypeId {
        let canonical = self.types.canonical(elem);
        let bad = if is_void(&self.types, canonical) {
            Some(("as array of voids", "E0532"))
        } else if is_function(&self.types, canonical) {
            Some(("as array of functions", "E0533"))
        } else {
            None
        };
        if let Some((what, code)) = bad {
            let who = self.declaration_of(subject);
            self.report(Diagnostic::error(format!("{who} {what}"), subject.span).with_code(code));
            return elem;
        }
        if !is_complete(&self.types, canonical) {
            let spelled = self.spell(elem);
            self.report(
                Diagnostic::error(
                    format!("array type has incomplete element type '{spelled}'"),
                    subject.span,
                )
                .with_code("E0534"),
            );
            return elem;
        }
        let len = self.array_len(elem, size, subject, place);
        self.types.array(elem, len)
    }

    /// How many elements an array has, which is four different answers and three diagnostics.
    fn array_len(
        &mut self,
        elem: TypeId,
        size: ArraySize,
        subject: Subject,
        place: Place,
    ) -> ArrayLen {
        let expr = match size {
            ArraySize::Unspecified => return ArrayLen::Unknown,
            ArraySize::Star if place.prototype => return ArrayLen::Star,
            ArraySize::Star => {
                self.report(
                    Diagnostic::error(
                        "'[*]' not allowed in other than function prototype scope".to_string(),
                        subject.span,
                    )
                    .with_code("E0539"),
                );
                return ArrayLen::Unknown;
            }
            ArraySize::Expr(expr) => expr,
        };

        let value = self.expr(expr);
        if self.is_poisoned(value) {
            return ArrayLen::Unknown;
        }
        // As a value, since this is a size and not an object, and because the same node is what
        // `sizeof` of the array is built out of: the walk to the IR evaluates it once where the
        // declaration is and answers with that afterwards, which only works if both are it.
        let value = self.value(value);
        let span = self.tast.expr_span(value);
        if !is_integer(&self.types, self.types.canonical(self.tast[value].ty)) {
            self.report(
                Diagnostic::error("size of array has non-integer type".to_string(), span)
                    .with_code("E0535"),
            );
            return ArrayLen::Unknown;
        }

        match self.eval_integer(value) {
            Ok(count) if count < 0 => {
                let who = self.array_named(subject);
                self.report(
                    Diagnostic::error(format!("size of {who} is negative"), span)
                        .with_code("E0536"),
                );
                ArrayLen::Unknown
            }
            Ok(count) => {
                let count = u64::try_from(count).unwrap_or(u64::MAX);
                if self.too_large(elem, count) {
                    let who = self.array_named(subject);
                    let message =
                        format!("size of {who} exceeds maximum object size '{MAX_OBJECT_SIZE}'");
                    self.report(Diagnostic::error(message, span).with_code("E0537"));
                    return ArrayLen::Unknown;
                }
                ArrayLen::Fixed(count)
            }
            // A size that is not a constant is a variable length array, which is a type and not
            // an error, except where there is no run time to evaluate it in.
            Err(failure) => {
                if failure.poisoned {
                    return ArrayLen::Unknown;
                }
                if self.scopes.at_file_scope() {
                    let who = match subject.name {
                        Some(name) => format!("'{}'", self.text(name)),
                        None => "type name".to_string(),
                    };
                    self.report(
                        Diagnostic::error(
                            format!("variably modified {who} at file scope"),
                            subject.span,
                        )
                        .with_code("E0538"),
                    );
                    return ArrayLen::Unknown;
                }
                ArrayLen::Variable(self.tast.add_vla(value))
            }
        }
    }

    /// Whether an array of this many of these does not fit in an object.
    fn too_large(&self, elem: TypeId, count: u64) -> bool {
        let Ok(elem) = layout(&self.types, elem, self.cx.target) else {
            return false;
        };
        // A zero sized element is a GNU empty structure, and any number of them is nothing.
        elem.size != 0 && count > MAX_OBJECT_SIZE / elem.size
    }

    /// A function taking these parameters and returning the type the steps arrived at.
    fn function_of(
        &mut self,
        ret: TypeId,
        params: ast::ParamList,
        variadic: bool,
        kind: ParamKind,
        subject: Subject,
    ) -> TypeId {
        let canonical = self.types.canonical(ret);
        let bad = if rucc_types::is_array(&self.types, canonical) {
            Some(("an array", "E0542"))
        } else if is_function(&self.types, canonical) {
            Some(("a function", "E0541"))
        } else {
            None
        };
        let ret = match bad {
            Some((what, code)) => {
                let who = self.declared_as(subject);
                self.report(
                    Diagnostic::error(format!("{who} as function returning {what}"), subject.span)
                        .with_code(code),
                );
                self.int()
            }
            None => ret,
        };

        let (params, prototyped) = match kind {
            ParamKind::Void => (Vec::new(), true),
            // `int f()` says nothing about the parameters before C23 and says there are none
            // from C23 onwards, and which of those it means is visible in every call.
            ParamKind::Empty => (Vec::new(), self.cx.std == Std::C23),
            // An old-style definition's identifier list has no types in it at all. They arrive
            // in the declarations between the parenthesis and the body, which is the function
            // definition's business rather than this one's.
            ParamKind::Identifiers => (Vec::new(), false),
            ParamKind::Prototype => (self.prototype(params), true),
        };
        self.types.function(FunctionType { ret, params, variadic, prototyped })
    }

    /// The parameter types of a prototype, adjusted the way a parameter is.
    fn prototype(&mut self, params: ast::ParamList) -> Vec<TypeId> {
        let ast = self.ast;
        let list = &ast[params];
        // A prototype is a scope of its own, which is what makes the `n` in
        // `void f(int n, int a[n])` mean the parameter and what makes it gone by the next
        // declaration. A parameter is declared after its own type is built, since a name is not
        // in scope for the declarator that declares it.
        self.scopes.push();
        let mut types = Vec::with_capacity(list.len());
        let mut declared = Vec::new();
        for (index, param) in list.iter().enumerate() {
            let ty = match param.specs {
                Some(specs) => self.build_type(
                    specs,
                    param.declarator,
                    Place { parameter: true, member: false, prototype: true },
                ),
                // An identifier list, which the caller told apart by its kind and which cannot
                // reach here. Its parameters have no specifiers at all.
                None => self.int(),
            };
            let declarator = ast[param.declarator];
            let span = if declarator.name.is_some() { declarator.name_span } else { param.span };
            self.check_void_parameter(ty, declarator.name, index, span);

            let adjusted = adjust_parameter(&mut self.types, ty);
            // The qualifiers in `int a[const 3]` are the pointer's, since the array is not what
            // the parameter has: they were written inside the brackets and belong outside them.
            let adjusted = match ast[declarator.derived].first() {
                Some(&Derived::Array { quals, .. }) => self.qualify(adjusted, quals, span),
                _ => adjusted,
            };
            types.push(adjusted);

            if let Some(name) = declarator.name {
                if self.scopes.lookup_here(name).is_some() {
                    let spelled = self.text(name).to_owned();
                    self.report(
                        Diagnostic::error(format!("redefinition of parameter '{spelled}'"), span)
                            .with_code("E0545"),
                    );
                } else {
                    // The adjusted type and not the written one, because that is what the
                    // parameter is: `sizeof a` inside `void f(int a[3])` is the size of a
                    // pointer, and a compiler that declares the array here says twelve.
                    declared.push(self.declare_object(name, adjusted, span));
                }
            }
        }
        self.scopes.pop();
        if let Some(first) = params.iter().next() {
            self.built.params.insert(first, declared);
        }
        types
    }

    /// The parameters a prototype declared, for the definition that binds them again.
    ///
    /// Empty for a list this has not seen, which is every list that is not a prototype.
    pub(in crate::check) fn prototype_params(&self, params: ast::ParamList) -> Vec<DeclId> {
        params
            .iter()
            .next()
            .and_then(|first| self.built.params.get(&first))
            .cloned()
            .unwrap_or_default()
    }

    /// A parameter declared `void`, which is one thing when it stands alone and two mistakes
    /// otherwise.
    fn check_void_parameter(&mut self, ty: TypeId, name: Option<Symbol>, index: usize, span: Span) {
        if !is_void(&self.types, self.types.canonical(ty)) {
            return;
        }
        let position = index + 1;
        match name {
            Some(name) => {
                let spelled = self.text(name).to_owned();
                self.report(
                    Diagnostic::warning(
                        format!("parameter {position} ('{spelled}') has void type"),
                        span,
                    )
                    .with_code("E0544"),
                );
            }
            // `(void)` on its own is a parameter list and not a parameter, and the parser
            // already told the two apart, so an unnamed one here has something beside it.
            None => {
                self.report(
                    Diagnostic::error("'void' must be the only parameter".to_string(), span)
                        .with_code("E0543"),
                );
            }
        }
    }

    /// `declaration of 'a'`, or the abstract declarator's version of the same phrase.
    fn declaration_of(&self, subject: Subject) -> String {
        match subject.name {
            Some(name) => format!("declaration of '{}'", self.text(name)),
            None => "declaration of type name".to_string(),
        }
    }

    /// `'f' declared`, or the abstract declarator's version.
    fn declared_as(&self, subject: Subject) -> String {
        match subject.name {
            Some(name) => format!("'{}' declared", self.text(name)),
            None => "type name declared".to_string(),
        }
    }

    /// `array 'a'`, or the abstract declarator's version.
    fn array_named(&self, subject: Subject) -> String {
        match subject.name {
            Some(name) => format!("array '{}'", self.text(name)),
            None => "unnamed array".to_string(),
        }
    }

    /// Reports something the type builder does not do yet.
    fn unsupported_type(&mut self, what: &str, span: Span) {
        self.report(
            Diagnostic::error(format!("{what} is not supported yet"), span).with_code("E0519"),
        );
    }

    /// A type the target does not have, which is not the same thing as one not written yet.
    fn unavailable_type(&mut self, name: &str, span: Span) {
        self.report(
            Diagnostic::error(format!("'{name}' is not supported on this target"), span)
                .with_code("E0589"),
        );
    }
}

/// The integer type a built-in names, if it names one.
fn int_kind(scalar: Scalar) -> Option<IntKind> {
    // `bool` is missing on purpose. It is an integer type in C and its own kind here, since it
    // is the one integer type where the conversion is not a truncation.
    let kind = match scalar {
        Scalar::Char => IntKind::Char,
        Scalar::SignedChar => IntKind::SChar,
        Scalar::UnsignedChar => IntKind::UChar,
        Scalar::Short => IntKind::Short,
        Scalar::UnsignedShort => IntKind::UShort,
        Scalar::Int => IntKind::Int,
        Scalar::UnsignedInt => IntKind::UInt,
        Scalar::Long => IntKind::Long,
        Scalar::UnsignedLong => IntKind::ULong,
        Scalar::LongLong => IntKind::LongLong,
        Scalar::UnsignedLongLong => IntKind::ULongLong,
        Scalar::Int128 => IntKind::Int128,
        Scalar::UnsignedInt128 => IntKind::UInt128,
        _ => return None,
    };
    Some(kind)
}

/// The floating type a built-in names, if the target has it.
///
/// The two that depend on the target are the ones spelled for a format rather than for a rank.
/// `__float80` is gcc's name for the x87 type and exists only where that type does, and there
/// it is the same type as `long double` rather than a second one beside it, which is what gcc
/// makes it and what `_Generic` can be used to see. `_Float128x` is a type no target gcc
/// supports has at all, which is why it is missing here rather than mapped to something.
fn float_kind(scalar: Scalar, target: &TargetInfo) -> Option<FloatKind> {
    match scalar {
        Scalar::Float => Some(FloatKind::Float),
        Scalar::Double => Some(FloatKind::Double),
        Scalar::LongDouble => Some(FloatKind::LongDouble),
        Scalar::Float16 => Some(FloatKind::Float16),
        Scalar::Float32 => Some(FloatKind::Float32),
        Scalar::Float64 => Some(FloatKind::Float64),
        Scalar::Float128 => Some(FloatKind::Float128),
        Scalar::Float32x => Some(FloatKind::Float32x),
        Scalar::Float64x => Some(FloatKind::Float64x),
        Scalar::Float80 if target.long_double_format == Format::X87Extended => {
            Some(FloatKind::LongDouble)
        }
        _ => None,
    }
}

/// How a built-in type is spelled, for the ones that have no type to be given.
fn spell_scalar(scalar: Scalar) -> &'static str {
    match scalar {
        Scalar::Void => "void",
        Scalar::Bool => "bool",
        Scalar::Char => "char",
        Scalar::SignedChar => "signed char",
        Scalar::UnsignedChar => "unsigned char",
        Scalar::Short => "short",
        Scalar::UnsignedShort => "unsigned short",
        Scalar::Int => "int",
        Scalar::UnsignedInt => "unsigned int",
        Scalar::Long => "long",
        Scalar::UnsignedLong => "unsigned long",
        Scalar::LongLong => "long long",
        Scalar::UnsignedLongLong => "unsigned long long",
        Scalar::Int128 => "__int128",
        Scalar::UnsignedInt128 => "unsigned __int128",
        // Without the width, which is an expression rather than a number until it is checked
        // and which no message this spells out has room for.
        Scalar::BitInt { unsigned: false, .. } => "_BitInt",
        Scalar::BitInt { unsigned: true, .. } => "unsigned _BitInt",
        Scalar::Float => "float",
        Scalar::Double => "double",
        Scalar::LongDouble => "long double",
        Scalar::Float16 => "_Float16",
        Scalar::Float32 => "_Float32",
        Scalar::Float64 => "_Float64",
        Scalar::Float128 => "_Float128",
        Scalar::Float32x => "_Float32x",
        Scalar::Float64x => "_Float64x",
        Scalar::Float128x => "_Float128x",
        Scalar::Float80 => "__float80",
        Scalar::Decimal32 => "_Decimal32",
        Scalar::Decimal64 => "_Decimal64",
        Scalar::Decimal128 => "_Decimal128",
    }
}

/// The fixture the child module's tests use as well, which is why several of the helpers
/// below are visible outside this module.
#[cfg(test)]
mod tests {
    use rucc_ast::{Builtin, BuiltinSet, DeclSpecs, DeclSpecsId, Declarator, DeclaratorId, Quals};
    use rucc_base::Interner;
    use rucc_lex::{IntConstant, IntConstantType, Remarks};
    use rucc_target::{TargetInfo, Triple};
    use rucc_types::{TypeKind, spell};

    use super::*;
    use crate::check::Context;

    /// The untyped tree a test checks, built by hand.
    ///
    /// The same shape as the expression tests next door and for the same reason: the checker
    /// borrows the interner for as long as it lives, so everything a test needs to name is
    /// named before the checker exists.
    pub(super) struct Fixture {
        pub(super) ast: rucc_ast::Ast,
        names: Interner,
        target: TargetInfo,
    }

    impl Fixture {
        pub(super) fn new() -> Fixture {
            Fixture::for_target("x86_64-unknown-linux-gnu")
        }

        /// The same, for a test whose answer is a property of the target.
        pub(super) fn for_target(triple: &str) -> Fixture {
            let target = TargetInfo::new(triple.parse::<Triple>().expect("a triple"));
            Fixture { ast: rucc_ast::Ast::new(), names: Interner::new(), target }
        }

        pub(super) fn name(&mut self, text: &str) -> Symbol {
            self.names.intern(text)
        }

        /// A specifier list naming a built-in type, as the keywords that were written.
        pub(super) fn keywords(&mut self, written: &[BuiltinSet]) -> DeclSpecsId {
            let mut builtin = Builtin::NONE;
            for &keyword in written {
                builtin = builtin.add(keyword).expect("a keyword written once");
            }
            self.specs(TypeSpec::Builtin(builtin), Quals::NONE)
        }

        /// `int`, which is what most of these declarations are made of.
        pub(super) fn int_specs(&mut self) -> DeclSpecsId {
            self.keywords(&[BuiltinSet::INT])
        }

        pub(super) fn specs(&mut self, ty: TypeSpec, quals: Quals) -> DeclSpecsId {
            let mut specs = DeclSpecs::empty(Span::DUMMY);
            specs.ty = ty;
            specs.quals = quals;
            self.ast.add_specs(specs)
        }

        pub(super) fn declarator(
            &mut self,
            name: Option<&str>,
            derived: &[Derived],
        ) -> DeclaratorId {
            let name = name.map(|text| self.name(text));
            let derived = self.ast.add_derived_list(derived);
            self.ast.add_declarator(Declarator {
                name,
                name_span: Span::DUMMY,
                derived,
                span: Span::DUMMY,
            })
        }

        /// A type name, which is a specifier list and an abstract declarator.
        pub(super) fn type_name(
            &mut self,
            specs: DeclSpecsId,
            derived: &[Derived],
        ) -> ast::TypeNameId {
            let declarator = self.declarator(None, derived);
            self.ast.add_type_name(ast::TypeName { specs, declarator, span: Span::DUMMY })
        }

        /// An integer constant, for the array bounds and the `_BitInt` widths.
        pub(super) fn int(&mut self, value: u128) -> ast::ExprId {
            let ty = IntConstantType::Standard(IntKind::Int);
            let id = self.ast.add_int(IntConstant { value, ty, remarks: Remarks::default() });
            self.ast.expr(ast::Expr::Int(id), Span::DUMMY)
        }

        fn use_name(&mut self, text: &str) -> ast::ExprId {
            let name = self.name(text);
            self.ast.expr(ast::Expr::Name(name), Span::DUMMY)
        }

        pub(super) fn checker(&self) -> Checker<'_> {
            Checker::new(&self.ast, Context::new(&self.names, &self.target, Std::C23))
        }
    }

    /// A fixed array bound, which is the common case and three lines every time.
    fn fixed(fixture: &mut Fixture, count: u128) -> Derived {
        let size = fixture.int(count);
        Derived::Array { size: ArraySize::Expr(size), quals: Quals::NONE, has_static: false }
    }

    /// A pointer with no qualifiers on it.
    fn pointer() -> Derived {
        Derived::Pointer { quals: Quals::NONE, attrs: rucc_ast::AttrList::EMPTY }
    }

    /// `_BitInt(width)`, with `unsigned` written next to it or not.
    fn bit_int(width: ast::ExprId, unsigned: bool) -> TypeSpec {
        let mut builtin = Builtin::NONE.add_bit_int(width).expect("`_BitInt` rejected");
        if unsigned {
            builtin = builtin.add(BuiltinSet::UNSIGNED).expect("`unsigned` rejected");
        }
        TypeSpec::Builtin(builtin)
    }

    /// How a built type is written, which is what almost every assertion here is about.
    pub(super) fn spelled(checker: &Checker<'_>, ty: TypeId) -> String {
        spell(&checker.types, checker.cx.names, ty)
    }

    /// The type a declaration declares, as it would be written.
    fn built(checker: &mut Checker<'_>, specs: DeclSpecsId, declarator: DeclaratorId) -> String {
        let ty = checker.declared_type(specs, declarator);
        spelled(checker, ty)
    }

    /// What was reported, as the messages alone.
    pub(super) fn messages(checker: &Checker<'_>) -> Vec<String> {
        checker.errors.diagnostics().iter().map(|d| d.message.clone()).collect()
    }

    /// The one message that was reported, which is what most of these tests expect.
    pub(super) fn message(checker: &Checker<'_>) -> String {
        let mut reported = messages(checker);
        assert_eq!(reported.len(), 1, "expected exactly one diagnostic, got {reported:?}");
        reported.pop().expect("one message")
    }

    #[test]
    fn the_keywords_of_a_specifier_list_name_one_type_between_them() {
        let mut fixture = Fixture::new();
        let long = fixture.keywords(&[BuiltinSet::UNSIGNED, BuiltinSet::LONG, BuiltinSet::INT]);
        let double = fixture.keywords(&[BuiltinSet::LONG, BuiltinSet::DOUBLE]);
        let void = fixture.keywords(&[BuiltinSet::VOID]);
        let plain = fixture.declarator(Some("x"), &[]);

        let mut checker = fixture.checker();
        assert_eq!(built(&mut checker, long, plain), "unsigned long");
        assert_eq!(built(&mut checker, double, plain), "long double");
        assert_eq!(built(&mut checker, void, plain), "void");
        assert!(messages(&checker).is_empty());
    }

    #[test]
    fn each_spelling_of_a_floating_type_names_a_type_of_its_own() {
        let mut fixture = Fixture::new();
        let written = [
            (BuiltinSet::FLOAT16, "_Float16"),
            (BuiltinSet::FLOAT32, "_Float32"),
            (BuiltinSet::FLOAT64, "_Float64"),
            (BuiltinSet::FLOAT128, "_Float128"),
            (BuiltinSet::FLOAT32X, "_Float32x"),
            (BuiltinSet::FLOAT64X, "_Float64x"),
        ];
        let specs: Vec<_> =
            written.iter().map(|&(keyword, _)| fixture.keywords(&[keyword])).collect();
        // `__float80` is gcc's name for the x87 type on the target that has one, and there it
        // is the same type as `long double` rather than a second type beside it.
        let float80 = fixture.keywords(&[BuiltinSet::FLOAT80]);
        let plain = fixture.declarator(Some("x"), &[]);

        let mut checker = fixture.checker();
        for (specs, expected) in specs.into_iter().zip(written.iter().map(|&(_, name)| name)) {
            assert_eq!(built(&mut checker, specs, plain), expected);
        }
        assert_eq!(built(&mut checker, float80, plain), "long double");
        assert!(messages(&checker).is_empty());
    }

    #[test]
    fn a_floating_type_the_target_does_not_have_is_refused_rather_than_given_another_one() {
        // `_Float128x` is a type no target gcc supports has at all, and `__float80` is one that
        // only x86 has. gcc turns both of them away in the same words, and the wording is worth
        // keeping apart from the one for a type this compiler has not written yet: nothing here
        // is coming later, the machine does not have the type.
        let mut fixture = Fixture::for_target("aarch64-apple-darwin");
        let float128x = fixture.keywords(&[BuiltinSet::FLOAT128X]);
        let float80 = fixture.keywords(&[BuiltinSet::FLOAT80]);
        let plain = fixture.declarator(Some("x"), &[]);

        let mut checker = fixture.checker();
        // A `double`, so that the declaration is still a declaration and the uses of the name
        // that follow are one error rather than one each.
        assert_eq!(built(&mut checker, float128x, plain), "double");
        assert_eq!(built(&mut checker, float80, plain), "double");
        assert_eq!(
            messages(&checker),
            [
                "'_Float128x' is not supported on this target",
                "'__float80' is not supported on this target",
            ]
        );
    }

    #[test]
    fn a_decimal_floating_type_is_recognised_and_says_it_is_not_written_yet() {
        // Deferred past 1.0 by `spec/19-open-questions.md`. The keyword is in the table and the
        // parser takes it, so the message has to be the one that says so rather than the one
        // for a keyword nobody has heard of.
        let mut fixture = Fixture::new();
        let specs = fixture.keywords(&[BuiltinSet::DECIMAL64]);
        let plain = fixture.declarator(Some("x"), &[]);

        let mut checker = fixture.checker();
        assert_eq!(built(&mut checker, specs, plain), "double");
        assert_eq!(message(&checker), "the type `_Decimal64` is not supported yet");
    }

    #[test]
    fn keywords_that_name_no_type_between_them_are_one_message_and_not_one_per_keyword() {
        let mut fixture = Fixture::new();
        // `short double`, which gcc reports once at the specifier list rather than at the
        // keyword, because the keyword that was wrong depends on which one was meant.
        let specs = fixture.keywords(&[BuiltinSet::SHORT, BuiltinSet::DOUBLE]);
        let plain = fixture.declarator(Some("x"), &[]);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(specs, plain);
        assert_eq!(spelled(&checker, ty), "int");
        assert_eq!(message(&checker), "two or more data types in declaration specifiers");
    }

    #[test]
    fn a_declaration_with_no_type_at_all_is_an_int_and_a_warning_that_says_whose() {
        let mut fixture = Fixture::new();
        // Two declarations rather than two declarators of one, since the specifiers of one
        // declaration are read once however many names it declares.
        let specs = fixture.specs(TypeSpec::None, Quals::CONST);
        let again = fixture.specs(TypeSpec::None, Quals::NONE);
        let named = fixture.declarator(Some("x"), &[]);
        let abstracted = fixture.declarator(None, &[]);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(specs, named);
        assert_eq!(spelled(&checker, ty), "const int");
        checker.declared_type(again, abstracted);
        assert_eq!(
            messages(&checker),
            ["type defaults to 'int' in declaration of 'x'", "type defaults to 'int'"]
        );
    }

    #[test]
    fn a_declarator_is_folded_from_the_far_end_so_the_step_nearest_the_name_wins() {
        let mut fixture = Fixture::new();
        let specs = fixture.int_specs();
        let char_specs = fixture.keywords(&[BuiltinSet::CHAR]);
        let parameter = fixture.declarator(None, &[]);
        let params = fixture.ast.add_param_list(&[ast::Param {
            specs: Some(char_specs),
            declarator: parameter,
            attrs: rucc_ast::AttrList::EMPTY,
            span: Span::DUMMY,
        }]);
        // `int (*f[3])(char)`: an array of three pointers to functions, which is the order the
        // derivations are written in and the reverse of the order they are applied.
        let three = fixed(&mut fixture, 3);
        let call = Derived::Function { params, variadic: false, kind: ParamKind::Prototype };
        let f = fixture.declarator(Some("f"), &[three, pointer(), call]);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(specs, f);
        assert_eq!(spelled(&checker, ty), "int (*[3])(char)");
        assert!(messages(&checker).is_empty());
    }

    #[test]
    fn the_qualifiers_of_a_pointer_are_the_pointers_and_not_the_pointees() {
        let mut fixture = Fixture::new();
        let konst = fixture.specs(
            TypeSpec::Builtin(Builtin::NONE.add(BuiltinSet::INT).expect("int")),
            Quals::CONST,
        );
        let plain = fixture.int_specs();
        // `const int *p`, which is a pointer to a constant.
        let to_const = fixture.declarator(Some("p"), &[pointer()]);
        // `int *const p`, which is a constant pointer.
        let const_pointer = fixture.declarator(
            Some("p"),
            &[Derived::Pointer { quals: Quals::CONST, attrs: rucc_ast::AttrList::EMPTY }],
        );

        let mut checker = fixture.checker();
        assert_eq!(built(&mut checker, konst, to_const), "const int *");
        assert_eq!(built(&mut checker, plain, const_pointer), "int *const");
        assert!(messages(&checker).is_empty());
    }

    #[test]
    fn restrict_is_only_for_a_pointer_and_says_so_where_it_is_not() {
        let mut fixture = Fixture::new();
        let specs = fixture.specs(TypeSpec::None, Quals::RESTRICT);
        let plain = fixture.declarator(Some("x"), &[]);
        let restricted =
            Derived::Pointer { quals: Quals::RESTRICT, attrs: rucc_ast::AttrList::EMPTY };
        let int_specs = fixture.int_specs();
        let p = fixture.declarator(Some("p"), &[restricted]);

        let mut checker = fixture.checker();
        assert_eq!(built(&mut checker, int_specs, p), "int *restrict");
        checker.declared_type(specs, plain);
        assert!(
            messages(&checker).contains(&"invalid use of 'restrict'".to_string()),
            "got {:?}",
            messages(&checker)
        );
    }

    #[test]
    fn an_array_of_something_there_can_be_no_array_of_says_which_it_was() {
        let mut fixture = Fixture::new();
        let void = fixture.keywords(&[BuiltinSet::VOID]);
        let int = fixture.int_specs();
        let three = fixed(&mut fixture, 3);
        let params = fixture.ast.add_param_list(&[]);
        let call = Derived::Function { params, variadic: false, kind: ParamKind::Void };

        let voids = fixture.declarator(Some("a"), &[three]);
        let functions = fixture.declarator(Some("a"), &[three, call]);
        let anonymous = fixture.declarator(None, &[three]);

        let mut checker = fixture.checker();
        checker.declared_type(void, voids);
        checker.declared_type(int, functions);
        checker.declared_type(void, anonymous);
        assert_eq!(
            messages(&checker),
            [
                "declaration of 'a' as array of voids",
                "declaration of 'a' as array of functions",
                "declaration of type name as array of voids",
            ]
        );
    }

    #[test]
    fn an_array_of_a_tag_that_has_no_definition_yet_names_the_type_it_cannot_size() {
        let mut fixture = Fixture::new();
        let tag = fixture.name("S");
        let specs = fixture.specs(
            TypeSpec::Record {
                kind: ast::RecordKind::Struct,
                tag: Some(tag),
                fields: None,
                attrs: rucc_ast::AttrList::EMPTY,
            },
            Quals::NONE,
        );
        let three = fixed(&mut fixture, 3);
        let array = fixture.declarator(Some("a"), &[three]);
        let star = fixture.declarator(Some("p"), &[pointer()]);

        let mut checker = fixture.checker();
        // A pointer to an incomplete type is perfectly ordinary, and both mentions of the tag
        // are the same type, which is what makes the pointer usable once the definition lands.
        let pointer_ty = checker.declared_type(specs, star);
        assert_eq!(spelled(&checker, pointer_ty), "struct S *");
        checker.declared_type(specs, array);
        assert_eq!(message(&checker), "array type has incomplete element type 'struct S'");
    }

    #[test]
    fn an_array_bound_is_folded_and_a_negative_one_is_refused() {
        let mut fixture = Fixture::new();
        let specs = fixture.int_specs();
        let zero = fixed(&mut fixture, 0);
        let four = fixed(&mut fixture, 4);
        let negative = {
            let one = fixture.int(1);
            let size = fixture
                .ast
                .expr(ast::Expr::Unary { op: rucc_ast::UnaryOp::Minus, operand: one }, Span::DUMMY);
            Derived::Array { size: ArraySize::Expr(size), quals: Quals::NONE, has_static: false }
        };
        let sized = fixture.declarator(Some("a"), &[four]);
        // `int a[0]`, which gcc accepts in silence as an extension and which a great deal of
        // real code uses as a flexible array member before C99 gave it a spelling.
        let empty = fixture.declarator(Some("a"), &[zero]);
        let unspecified = fixture.declarator(
            Some("a"),
            &[Derived::Array {
                size: ArraySize::Unspecified,
                quals: Quals::NONE,
                has_static: false,
            }],
        );
        let backwards = fixture.declarator(Some("a"), &[negative]);

        let mut checker = fixture.checker();
        assert_eq!(built(&mut checker, specs, sized), "int [4]");
        assert_eq!(built(&mut checker, specs, empty), "int [0]");
        assert_eq!(built(&mut checker, specs, unspecified), "int []");
        assert!(messages(&checker).is_empty());

        checker.declared_type(specs, backwards);
        assert_eq!(message(&checker), "size of array 'a' is negative");
    }

    #[test]
    fn an_array_too_large_to_be_an_object_is_measured_in_its_elements() {
        let mut fixture = Fixture::new();
        let specs = fixture.int_specs();
        // Four times this is one element past the largest object, and the count on its own is
        // not, which is what makes the check about the element type and not about the bound.
        let count = u128::from(MAX_OBJECT_SIZE / 4 + 1);
        let huge = fixed(&mut fixture, count);
        let a = fixture.declarator(Some("a"), &[huge]);

        let mut checker = fixture.checker();
        checker.declared_type(specs, a);
        assert_eq!(
            message(&checker),
            "size of array 'a' exceeds maximum object size '9223372036854775807'"
        );
    }

    #[test]
    fn a_bound_that_is_not_a_constant_is_a_variable_length_array_where_there_is_a_run_time() {
        let mut fixture = Fixture::new();
        let specs = fixture.int_specs();
        let n = fixture.use_name("n");
        let variable =
            Derived::Array { size: ArraySize::Expr(n), quals: Quals::NONE, has_static: false };
        let a = fixture.declarator(Some("a"), &[variable]);
        let name = fixture.name("n");

        let mut checker = fixture.checker();
        let int = checker.int();
        checker.declare_object(name, int, Span::DUMMY);
        // At file scope there is nothing to evaluate the bound in, which is the error gcc
        // gives, and the type is not built rather than built wrong.
        checker.declared_type(specs, a);
        assert_eq!(message(&checker), "variably modified 'a' at file scope");

        checker.scopes.push();
        let ty = checker.declared_type(specs, a);
        assert_eq!(spelled(&checker, ty), "int [*]");
        // Two arrays written the same way are still two types, because the two bounds are
        // evaluated at two different moments and may not agree.
        let again = checker.declared_type(specs, a);
        assert_ne!(ty, again);
        assert_eq!(
            checker.tast.vla_size(vla_id(&checker, ty)),
            checker.tast.vla_size(vla_id(&checker, ty))
        );
    }

    /// The identity of the variable length array a type is, which the assertions above want.
    fn vla_id(checker: &Checker<'_>, ty: TypeId) -> rucc_types::VlaId {
        match checker.types.kind(checker.types.canonical(ty)) {
            TypeKind::Array { len: ArrayLen::Variable(id), .. } => id,
            other => panic!("expected a variable length array, got {other:?}"),
        }
    }

    #[test]
    fn a_star_bound_is_only_a_type_inside_a_prototype() {
        let mut fixture = Fixture::new();
        let specs = fixture.int_specs();
        let star = Derived::Array { size: ArraySize::Star, quals: Quals::NONE, has_static: false };
        let parameter = fixture.declarator(Some("a"), &[star]);
        let params = fixture.ast.add_param_list(&[ast::Param {
            specs: Some(specs),
            declarator: parameter,
            attrs: rucc_ast::AttrList::EMPTY,
            span: Span::DUMMY,
        }]);
        let call = Derived::Function { params, variadic: false, kind: ParamKind::Prototype };
        let f = fixture.declarator(Some("f"), &[call]);

        let mut checker = fixture.checker();
        // Inside the prototype it is a type, and the parameter it is on is adjusted to a
        // pointer the same way any other array parameter is.
        let ty = checker.declared_type(specs, f);
        assert_eq!(spelled(&checker, ty), "int (int *)");
        assert!(messages(&checker).is_empty());

        checker.declared_type(specs, parameter);
        assert_eq!(message(&checker), "'[*]' not allowed in other than function prototype scope");
    }

    #[test]
    fn a_deduced_type_on_a_parameter_names_nothing_and_says_where_it_was_written() {
        let mut fixture = Fixture::new();
        // A parameter has no initializer to deduce from, so there is nothing here for either
        // spelling to mean and the parameter is an `int` so that the rest is still checked.
        let specs = fixture.int_specs();
        let deduced = fixture.specs(TypeSpec::Auto(ast::Deduction::Auto), Quals::NONE);
        let parameter = fixture.declarator(Some("p"), &[]);
        let params = fixture.ast.add_param_list(&[ast::Param {
            specs: Some(deduced),
            declarator: parameter,
            attrs: rucc_ast::AttrList::EMPTY,
            span: Span::DUMMY,
        }]);
        let call = Derived::Function { params, variadic: false, kind: ParamKind::Prototype };
        let f = fixture.declarator(Some("f"), &[call]);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(specs, f);
        assert_eq!(spelled(&checker, ty), "int (int)");
        assert_eq!(message(&checker), "'auto' not allowed in function prototype");
    }

    #[test]
    fn the_qualifiers_inside_a_parameters_brackets_end_up_on_the_pointer_it_becomes() {
        let mut fixture = Fixture::new();
        let specs = fixture.int_specs();
        let three = fixture.int(3);
        let qualified =
            Derived::Array { size: ArraySize::Expr(three), quals: Quals::CONST, has_static: true };
        let parameter = fixture.declarator(Some("a"), &[qualified]);
        let params = fixture.ast.add_param_list(&[ast::Param {
            specs: Some(specs),
            declarator: parameter,
            attrs: rucc_ast::AttrList::EMPTY,
            span: Span::DUMMY,
        }]);
        let call = Derived::Function { params, variadic: false, kind: ParamKind::Prototype };
        let f = fixture.declarator(Some("f"), &[call]);

        let mut checker = fixture.checker();
        // `void f(int a[static const 3])`, whose parameter is a `int *const`.
        let ty = checker.declared_type(specs, f);
        assert_eq!(spelled(&checker, ty), "int (int *const)");
        assert!(messages(&checker).is_empty());

        // The same brackets on something that is not a parameter mean nothing at all.
        checker.declared_type(specs, parameter);
        assert_eq!(
            message(&checker),
            "static or type qualifiers in non-parameter array declarator"
        );
    }

    #[test]
    fn a_function_cannot_return_a_function_or_an_array_and_the_message_names_which() {
        let mut fixture = Fixture::new();
        let specs = fixture.int_specs();
        let params = fixture.ast.add_param_list(&[]);
        let call = Derived::Function { params, variadic: false, kind: ParamKind::Void };
        let three = fixed(&mut fixture, 3);

        let returns_function = fixture.declarator(Some("f"), &[call, call]);
        let returns_array = fixture.declarator(Some("f"), &[call, three]);
        let anonymous = fixture.declarator(None, &[call, three]);

        let mut checker = fixture.checker();
        checker.declared_type(specs, returns_function);
        checker.declared_type(specs, returns_array);
        checker.declared_type(specs, anonymous);
        assert_eq!(
            messages(&checker),
            [
                "'f' declared as function returning a function",
                "'f' declared as function returning an array",
                "type name declared as function returning an array",
            ]
        );
    }

    #[test]
    fn an_empty_parameter_list_says_nothing_before_c23_and_says_none_from_it() {
        let mut fixture = Fixture::new();
        let specs = fixture.int_specs();
        let params = fixture.ast.add_param_list(&[]);
        let empty = Derived::Function { params, variadic: false, kind: ParamKind::Empty };
        let f = fixture.declarator(Some("f"), &[empty]);

        let mut checker = fixture.checker();
        assert_eq!(built(&mut checker, specs, f), "int (void)");

        let mut old = fixture.checker();
        old.cx.std = Std::C17;
        assert_eq!(built(&mut old, specs, f), "int ()");
        assert!(messages(&old).is_empty());
    }

    #[test]
    fn a_parameter_of_type_void_is_only_a_parameter_list_when_it_is_the_whole_of_one() {
        let mut fixture = Fixture::new();
        let int = fixture.int_specs();
        let void = fixture.keywords(&[BuiltinSet::VOID]);
        let named = fixture.declarator(Some("v"), &[]);
        let unnamed = fixture.declarator(None, &[]);
        let param = |declarator| ast::Param {
            specs: Some(void),
            declarator,
            attrs: rucc_ast::AttrList::EMPTY,
            span: Span::DUMMY,
        };
        let params = fixture.ast.add_param_list(&[param(named), param(unnamed)]);
        let call = Derived::Function { params, variadic: false, kind: ParamKind::Prototype };
        let f = fixture.declarator(Some("f"), &[call]);

        let mut checker = fixture.checker();
        checker.declared_type(int, f);
        assert_eq!(
            messages(&checker),
            ["parameter 1 ('v') has void type", "'void' must be the only parameter"]
        );
    }

    #[test]
    fn a_parameter_is_in_scope_for_the_parameters_after_it_and_gone_after_the_prototype() {
        let mut fixture = Fixture::new();
        let specs = fixture.int_specs();
        let n = fixture.declarator(Some("n"), &[]);
        let bound = fixture.use_name("n");
        let a = fixture.declarator(
            Some("a"),
            &[Derived::Array {
                size: ArraySize::Expr(bound),
                quals: Quals::NONE,
                has_static: false,
            }],
        );
        let param = |declarator| ast::Param {
            specs: Some(specs),
            declarator,
            attrs: rucc_ast::AttrList::EMPTY,
            span: Span::DUMMY,
        };
        let params = fixture.ast.add_param_list(&[param(n), param(a)]);
        let call = Derived::Function { params, variadic: false, kind: ParamKind::Prototype };
        let f = fixture.declarator(Some("f"), &[call]);
        let name = fixture.name("n");

        let mut checker = fixture.checker();
        // A prototype is a scope of its own, so the `n` in the bound is the parameter and the
        // whole thing is a prototype rather than a use of an undeclared name.
        let ty = checker.declared_type(specs, f);
        assert_eq!(spelled(&checker, ty), "int (int, int *)");
        assert!(messages(&checker).is_empty());
        assert!(checker.scopes.lookup(name).is_none());
    }

    #[test]
    fn a_parameter_declared_twice_in_one_prototype_is_reported_once() {
        let mut fixture = Fixture::new();
        let specs = fixture.int_specs();
        let a = fixture.declarator(Some("a"), &[]);
        let param = ast::Param {
            specs: Some(specs),
            declarator: a,
            attrs: rucc_ast::AttrList::EMPTY,
            span: Span::DUMMY,
        };
        let params = fixture.ast.add_param_list(&[param, param]);
        let call = Derived::Function { params, variadic: false, kind: ParamKind::Prototype };
        let f = fixture.declarator(Some("f"), &[call]);

        let mut checker = fixture.checker();
        checker.declared_type(specs, f);
        assert_eq!(message(&checker), "redefinition of parameter 'a'");
    }

    #[test]
    fn a_tag_names_the_same_type_every_time_and_one_kind_of_thing_only() {
        let mut fixture = Fixture::new();
        let tag = fixture.name("S");
        let record = |kind| TypeSpec::Record {
            kind,
            tag: Some(tag),
            fields: None,
            attrs: rucc_ast::AttrList::EMPTY,
        };
        let structure = fixture.specs(record(ast::RecordKind::Struct), Quals::NONE);
        let onion = fixture.specs(record(ast::RecordKind::Union), Quals::NONE);
        let plain = fixture.declarator(None, &[]);

        let mut checker = fixture.checker();
        let first = checker.declared_type(structure, plain);
        let second = checker.declared_type(structure, plain);
        assert_eq!(first, second);
        assert!(messages(&checker).is_empty());

        let wrong = checker.declared_type(onion, plain);
        assert_eq!(message(&checker), "'S' defined as wrong kind of tag");
        // The tag keeps meaning what it did, so the declarations after this one are checked
        // against the definition that is there rather than against a second one.
        assert_ne!(wrong, first);
        assert_eq!(checker.declared_type(structure, plain), first);
    }

    #[test]
    fn an_anonymous_tag_is_a_new_type_every_time_it_is_written() {
        let mut fixture = Fixture::new();
        let anonymous = |fixture: &mut Fixture| {
            fixture.specs(
                TypeSpec::Record {
                    kind: ast::RecordKind::Struct,
                    tag: None,
                    fields: None,
                    attrs: rucc_ast::AttrList::EMPTY,
                },
                Quals::NONE,
            )
        };
        let specs = anonymous(&mut fixture);
        let written_again = anonymous(&mut fixture);
        let plain = fixture.declarator(None, &[]);

        let mut checker = fixture.checker();
        let first = checker.declared_type(specs, plain);
        let second = checker.declared_type(written_again, plain);
        assert_ne!(first, second);
        // And one that was written once is one type however many names it declares, which is
        // what makes `struct { int x; } a, b;` two objects of the same type.
        assert_eq!(checker.declared_type(specs, plain), first);
    }

    #[test]
    fn an_enumeration_with_the_underlying_type_written_is_complete_from_there() {
        let mut fixture = Fixture::new();
        let long = fixture.keywords(&[BuiltinSet::LONG]);
        let long_name = fixture.type_name(long, &[]);
        let tag = fixture.name("E");
        let fixed_enum = fixture.specs(
            TypeSpec::Enum {
                tag: Some(tag),
                enumerators: None,
                underlying: Some(long_name),
                attrs: rucc_ast::AttrList::EMPTY,
            },
            Quals::NONE,
        );
        let plain = fixture.declarator(None, &[]);

        let mut checker = fixture.checker();
        let ty = checker.declared_type(fixed_enum, plain);
        assert_eq!(spelled(&checker, ty), "enum E");
        assert!(is_complete(&checker.types, ty));
        assert!(messages(&checker).is_empty());
    }

    #[test]
    fn an_enumeration_cannot_be_kept_in_something_that_is_not_an_integer_type() {
        let mut fixture = Fixture::new();
        let double = fixture.keywords(&[BuiltinSet::DOUBLE]);
        let double_name = fixture.type_name(double, &[]);
        let specs = fixture.specs(
            TypeSpec::Enum {
                tag: None,
                enumerators: None,
                underlying: Some(double_name),
                attrs: rucc_ast::AttrList::EMPTY,
            },
            Quals::NONE,
        );
        let plain = fixture.declarator(None, &[]);

        let mut checker = fixture.checker();
        checker.declared_type(specs, plain);
        assert_eq!(message(&checker), "invalid 'enum' underlying type");
    }

    #[test]
    fn atomic_is_a_type_and_not_a_qualifier_and_two_things_cannot_be_one() {
        let mut fixture = Fixture::new();
        let int = fixture.int_specs();
        let konst = fixture.specs(
            TypeSpec::Builtin(Builtin::NONE.add(BuiltinSet::INT).expect("int")),
            Quals::CONST,
        );
        let plain_name = fixture.type_name(int, &[]);
        let three = fixed(&mut fixture, 3);
        let array_name = fixture.type_name(int, &[three]);
        let params = fixture.ast.add_param_list(&[]);
        let call = Derived::Function { params, variadic: false, kind: ParamKind::Void };
        let function_name = fixture.type_name(int, &[call]);
        let const_name = fixture.type_name(konst, &[]);

        let atomic = |fixture: &mut Fixture, name| {
            let specs = fixture.specs(TypeSpec::Atomic(name), Quals::NONE);
            let declarator = fixture.declarator(None, &[]);
            (specs, declarator)
        };
        let (plain, hole) = atomic(&mut fixture, plain_name);
        let (array, _) = atomic(&mut fixture, array_name);
        let (function, _) = atomic(&mut fixture, function_name);
        let (qualified, _) = atomic(&mut fixture, const_name);

        let mut checker = fixture.checker();
        assert_eq!(built(&mut checker, plain, hole), "_Atomic(int)");
        assert!(messages(&checker).is_empty());

        checker.declared_type(array, hole);
        checker.declared_type(function, hole);
        checker.declared_type(qualified, hole);
        assert_eq!(
            messages(&checker),
            [
                "'_Atomic'-qualified array type",
                "'_Atomic'-qualified function type",
                "'_Atomic' applied to a qualified type",
            ]
        );
    }

    #[test]
    fn a_bit_int_is_as_wide_as_it_says_within_the_range_there_is() {
        let mut fixture = Fixture::new();
        let widths = [37, 1, 200, 0];
        let specs: Vec<_> = widths
            .iter()
            .map(|&width| {
                let expr = fixture.int(width);
                fixture.specs(bit_int(expr, false), Quals::NONE)
            })
            .collect();
        let plain = fixture.declarator(None, &[]);

        let mut checker = fixture.checker();
        assert_eq!(built(&mut checker, specs[0], plain), "_BitInt(37)");
        assert!(messages(&checker).is_empty());

        checker.declared_type(specs[1], plain);
        checker.declared_type(specs[2], plain);
        checker.declared_type(specs[3], plain);
        assert_eq!(
            messages(&checker),
            [
                "'signed _BitInt' argument must be at least 2",
                "'_BitInt' argument '200' is larger than 'BITINT_MAXWIDTH' '128'",
                "'_BitInt' argument '0' is not a positive integer constant expression",
            ]
        );
    }

    #[test]
    fn an_unsigned_bit_int_holds_one_bit_where_a_signed_one_cannot() {
        let mut fixture = Fixture::new();
        let one = fixture.int(1);
        let unsigned = fixture.specs(bit_int(one, true), Quals::NONE);
        let eight = fixture.int(8);
        let wide = fixture.specs(bit_int(eight, true), Quals::NONE);
        let plain = fixture.declarator(None, &[]);

        let mut checker = fixture.checker();
        assert_eq!(built(&mut checker, unsigned, plain), "unsigned _BitInt(1)");
        assert_eq!(built(&mut checker, wide, plain), "unsigned _BitInt(8)");
        assert!(messages(&checker).is_empty());
    }

    #[test]
    fn a_bit_int_next_to_anything_but_a_sign_names_no_type() {
        let mut fixture = Fixture::new();
        let width = fixture.int(8);
        let mut both = Builtin::NONE.add(BuiltinSet::LONG).expect("`long` rejected");
        both = both.add_bit_int(width).expect("`_BitInt` rejected");
        let specs = fixture.specs(TypeSpec::Builtin(both), Quals::NONE);
        let plain = fixture.declarator(None, &[]);

        let mut checker = fixture.checker();
        checker.declared_type(specs, plain);
        assert_eq!(messages(&checker), ["two or more data types in declaration specifiers"]);
    }

    #[test]
    fn a_typedef_name_is_the_type_it_was_declared_for_and_keeps_its_own_spelling() {
        let mut fixture = Fixture::new();
        let word = fixture.name("word");
        let specs = fixture.specs(TypeSpec::Typedef(word), Quals::CONST);
        let p = fixture.declarator(Some("p"), &[pointer()]);

        let mut checker = fixture.checker();
        let long = checker.types.int(IntKind::Long);
        let alias = checker.types.typedef(word, long);
        checker.declare_typedef(word, alias);

        let ty = checker.declared_type(specs, p);
        assert_eq!(spelled(&checker, ty), "const word *");
        assert!(messages(&checker).is_empty());
    }

    #[test]
    fn typeof_takes_the_type_of_an_expression_it_does_not_evaluate() {
        let mut fixture = Fixture::new();
        let x = fixture.use_name("x");
        let plain = fixture
            .specs(TypeSpec::Typeof { unqual: false, operand: TypeofArg::Expr(x) }, Quals::NONE);
        let bare = fixture
            .specs(TypeSpec::Typeof { unqual: true, operand: TypeofArg::Expr(x) }, Quals::NONE);
        let hole = fixture.declarator(None, &[]);
        let name = fixture.name("x");

        let mut checker = fixture.checker();
        let int = checker.int();
        let konst = checker.types.qualified(int, Qualifiers::CONST);
        checker.declare_object(name, konst, Span::DUMMY);

        assert_eq!(built(&mut checker, plain, hole), "const int");
        // `typeof_unqual` is the one that takes them off, which is what makes it worth having.
        assert_eq!(built(&mut checker, bare, hole), "int");
        assert!(messages(&checker).is_empty());
    }
}
