//! Declaration specifiers, attributes, and the struct and enum bodies written inside them.
//!
//! Design: `spec/06-lexer-and-parser.md` sections 6.4, 6.6 and 6.7.
//!
//! # Why the specifiers are a set and not a sequence
//!
//! `unsigned static const long int x;` is a legal spelling of `static const unsigned long x`,
//! so the keywords are accumulated into a record as they are read rather than kept in the order
//! they were written. The exception is the type keywords, which stay as the multiset that was
//! written and are turned into a type by [`Builtin::resolve`] later, because `long` means three
//! different things depending on what else is in the list and the parser has not seen the rest
//! of the list yet.
//!
//! # Where the specifier list stops
//!
//! At the first token that cannot continue it, and the only hard case is an identifier. An
//! identifier is a type specifier when the scopes say it is a typedef name and nothing has
//! named a type yet; otherwise it belongs to the declarator. That single rule is what makes
//! `typedef int T; T T;` declare a variable called `T`, and it is why the scope stack in
//! [`crate::scope`] has to be updated as the parse goes rather than afterwards.

use rucc_ast::{
    AlignSpec, AttrArg, AttrArgList, AttrList, AttrSyntax, Attribute, Builtin, BuiltinError,
    BuiltinSet, DeclSpecs, DeclSpecsId, Deduction, Enumerator, EnumeratorList, ExprId, Field,
    FuncSpecs, Member, MemberList, Quals, RecordKind, StorageClass, StrId, TypeSpec, TypeofArg,
};
use rucc_base::Symbol;
use rucc_diag::Span;
use rucc_lex::{Keyword, Punct, Token, TokenKind};
use rucc_session::Std;

use crate::cursor::MAX_LOOKAHEAD;
use crate::parser::Parser;
use crate::scope::{IdentKind, TagKind};

/// What a declaration that names two storage classes is told. The two are not named in it,
/// which is gcc's wording and is what a build that greps its log expects to find, and the span
/// points at the second of them, which is the one the writer can delete.
const MULTIPLE_STORAGE_CLASSES: &str = "multiple storage classes in declaration specifiers";

/// The built-in type the keyword names, for the keywords that name one.
fn builtin_keyword(word: Keyword) -> Option<BuiltinSet> {
    let set = match word {
        Keyword::Void => BuiltinSet::VOID,
        Keyword::Bool => BuiltinSet::BOOL,
        Keyword::Char => BuiltinSet::CHAR,
        Keyword::Short => BuiltinSet::SHORT,
        Keyword::Int => BuiltinSet::INT,
        Keyword::Long => BuiltinSet::LONG,
        Keyword::Signed => BuiltinSet::SIGNED,
        Keyword::Unsigned => BuiltinSet::UNSIGNED,
        Keyword::Float => BuiltinSet::FLOAT,
        Keyword::Double => BuiltinSet::DOUBLE,
        Keyword::Complex => BuiltinSet::COMPLEX,
        Keyword::Imaginary => BuiltinSet::IMAGINARY,
        Keyword::Int128 => BuiltinSet::INT128,
        // gcc's two are typedefs of `__int128` and of `unsigned __int128`, so each one
        // carries its own signedness rather than waiting for a `signed` or an `unsigned`
        // beside it. That also makes `unsigned __int128_t` the contradiction it is.
        Keyword::Int128T => BuiltinSet::INT128.with(BuiltinSet::SIGNED),
        Keyword::UInt128T => BuiltinSet::INT128.with(BuiltinSet::UNSIGNED),
        Keyword::Float16 => BuiltinSet::FLOAT16,
        Keyword::Float32 => BuiltinSet::FLOAT32,
        Keyword::Float64 => BuiltinSet::FLOAT64,
        Keyword::Float128 => BuiltinSet::FLOAT128,
        Keyword::Float32x => BuiltinSet::FLOAT32X,
        Keyword::Float64x => BuiltinSet::FLOAT64X,
        Keyword::Float128x => BuiltinSet::FLOAT128X,
        Keyword::Decimal32 => BuiltinSet::DECIMAL32,
        Keyword::Decimal64 => BuiltinSet::DECIMAL64,
        Keyword::Decimal128 => BuiltinSet::DECIMAL128,
        _ => return None,
    };
    Some(set)
}

/// The qualifier the keyword names.
fn qual_keyword(word: Keyword) -> Option<Quals> {
    match word {
        Keyword::Const => Some(Quals::CONST),
        Keyword::Volatile => Some(Quals::VOLATILE),
        Keyword::Restrict => Some(Quals::RESTRICT),
        _ => None,
    }
}

/// The storage class the keyword names.
fn storage_keyword(word: Keyword) -> Option<StorageClass> {
    match word {
        Keyword::Typedef => Some(StorageClass::Typedef),
        Keyword::Extern => Some(StorageClass::Extern),
        Keyword::Static => Some(StorageClass::Static),
        Keyword::Auto => Some(StorageClass::Auto),
        Keyword::Register => Some(StorageClass::Register),
        _ => None,
    }
}

/// The parts of a specifier list that are still being decided while it is read.
struct Pending<'a> {
    /// The list so far, which is everything already decided.
    specs: &'a mut DeclSpecs,
    /// The built-in type keywords, which are a multiset rather than one specifier each.
    builtin: &'a mut Builtin,
    /// Whether something other than those keywords has named a type, which is what stops a
    /// second one being read and what stops an identifier being read at all.
    named: &'a mut bool,
    /// The `auto` keywords, which are two different specifiers wearing one spelling.
    autos: &'a mut Autos,
}

/// The `auto` keywords of one specifier list.
#[derive(Clone, Copy)]
struct Autos {
    /// How many were written, since a second one is a duplicate whichever of the two it is.
    count: u32,
    /// Where the first one was, which is where anything said about them is reported.
    span: Span,
}

/// Whether the keyword can begin a type name, which is the specifiers minus the storage
/// classes and the function specifiers.
///
/// `__auto_type` is not one of them, and neither is `auto`. A type name has no declarator to
/// deduce a type from, so `sizeof(__auto_type)` and `(__auto_type)x` name nothing, which is
/// what gcc and clang both say about them. It still begins a declaration, which is
/// [`Parser::starts_decl_specs`]'s question and not this one.
fn type_keyword(word: Keyword) -> bool {
    if builtin_keyword(word).is_some() || qual_keyword(word).is_some() {
        return true;
    }
    matches!(
        word,
        Keyword::Struct
            | Keyword::Union
            | Keyword::Enum
            | Keyword::Atomic
            | Keyword::Typeof
            | Keyword::TypeofUnqual
            | Keyword::BitInt
            | Keyword::Attribute
            | Keyword::BuiltinVaList
    )
}

impl Parser<'_> {
    /// Whether `token` can begin a type name.
    ///
    /// This is the question the cast, the compound literal and `sizeof` all ask, and it is the
    /// one place the typedef decision is load bearing rather than convenient: with `A` a type
    /// name, `(A)*B` is a cast, and without it, a multiplication.
    pub(crate) fn starts_type_name(&self, token: Token) -> bool {
        match token.kind {
            TokenKind::Keyword(word) => type_keyword(word),
            TokenKind::Ident => self.scopes.is_typedef_name(Symbol::from_raw(token.value)),
            _ => false,
        }
    }

    /// Whether `token` can begin a declaration.
    pub(crate) fn starts_decl_specs(&self, token: Token) -> bool {
        match token.kind {
            TokenKind::Keyword(word) => {
                type_keyword(word)
                    || storage_keyword(word).is_some()
                    || matches!(
                        word,
                        Keyword::Inline
                            | Keyword::Noreturn
                            | Keyword::Alignas
                            | Keyword::ThreadLocal
                            | Keyword::Constexpr
                            | Keyword::AutoType
                    )
            }
            TokenKind::Ident => self.scopes.is_typedef_name(Symbol::from_raw(token.value)),
            _ => false,
        }
    }

    /// Whether a declaration begins at the cursor, looking past any `__extension__` in front of
    /// it.
    ///
    /// The keyword cannot be the thing that decides, because it is written in front of both
    /// kinds of thing: `__extension__ int x;` is a declaration and `__extension__ (x + 1);` is
    /// an expression statement, and what tells them apart is what comes after it.
    pub(crate) fn at_decl_specs(&self) -> bool {
        let mut ahead = 0;
        // A run of them, since one macro expanding in front of another is how two in a row
        // happens, and bounded because the cursor's lookahead is.
        while ahead < MAX_LOOKAHEAD && self.cursor.peek(ahead).keyword() == Some(Keyword::Extension)
        {
            ahead += 1;
        }
        self.starts_decl_specs(self.cursor.peek(ahead))
    }

    /// Whether the parser is looking at `[[`, which is C23's attribute syntax.
    ///
    /// Two tokens, because `[[` is not a punctuator: `a[[b] = 1]` would be one if it were, and
    /// while nobody writes that, the grammar allows it and a lexer that joined the brackets
    /// would get it wrong.
    pub(crate) fn at_standard_attribute(&self) -> bool {
        self.cursor.at_punct(Punct::LBracket)
            && self.cursor.peek(1).punct() == Some(Punct::LBracket)
    }

    /// Whether an attribute specifier of either syntax comes next.
    pub(crate) fn at_attribute(&self) -> bool {
        self.cursor.at_keyword(Keyword::Attribute) || self.at_standard_attribute()
    }

    /// Every attribute specifier written here, in either syntax, as one list.
    ///
    /// GCC allows the two spellings to be mixed and repeated in the same position, so this is a
    /// loop rather than a single specifier. Which syntax each one was written in is kept on the
    /// attribute, because the placement rules differ between them and a diagnostic that quotes
    /// the wrong spelling back is worse than no diagnostic.
    pub(crate) fn attributes(&mut self) -> AttrList {
        if !self.at_attribute() {
            return AttrList::EMPTY;
        }
        let mut attrs = Vec::new();
        self.collect_attributes(&mut attrs);
        self.ast.add_attr_list(&attrs)
    }

    /// Appends the attributes written here to `out`.
    fn collect_attributes(&mut self, out: &mut Vec<Attribute>) {
        loop {
            if self.at_standard_attribute() {
                self.standard_attributes(out);
            } else if self.cursor.at_keyword(Keyword::Attribute) {
                self.gnu_attributes(out);
            } else {
                return;
            }
        }
    }

    /// `[[ ... ]]`.
    fn standard_attributes(&mut self, out: &mut Vec<Attribute>) {
        self.cursor.bump();
        self.cursor.bump();
        loop {
            if self.cursor.at_punct(Punct::RBracket) || self.cursor.is_eof() {
                break;
            }
            let before = self.cursor.index();
            if let Some(attr) = self.one_attribute(AttrSyntax::Standard) {
                out.push(attr);
            }
            if !self.cursor.eat_punct(Punct::Comma) {
                break;
            }
            if self.cursor.index() == before {
                break;
            }
        }
        self.expect_punct(Punct::RBracket);
        self.expect_punct(Punct::RBracket);
    }

    /// `__attribute__(( ... ))`.
    fn gnu_attributes(&mut self, out: &mut Vec<Attribute>) {
        self.cursor.bump();
        if !self.expect_punct(Punct::LParen) {
            return;
        }
        if !self.expect_punct(Punct::LParen) {
            return;
        }
        loop {
            if self.cursor.at_punct(Punct::RParen) || self.cursor.is_eof() {
                break;
            }
            // `__attribute__((packed,))` is accepted by GCC, so an empty item is not an error.
            let before = self.cursor.index();
            if !self.cursor.at_punct(Punct::Comma) {
                if let Some(attr) = self.one_attribute(AttrSyntax::Gnu) {
                    out.push(attr);
                }
            }
            if !self.cursor.eat_punct(Punct::Comma) {
                break;
            }
            if self.cursor.index() == before {
                break;
            }
        }
        self.expect_punct(Punct::RParen);
        self.expect_punct(Punct::RParen);
    }

    /// One attribute, with its namespace and arguments.
    fn one_attribute(&mut self, syntax: AttrSyntax) -> Option<Attribute> {
        let start = self.cursor.span();
        let mut namespace = None;
        let mut name = self.attribute_name()?;
        if self.cursor.eat_punct(Punct::ColonColon) {
            namespace = Some(name);
            name = self.attribute_name()?;
        }
        let args = if self.cursor.at_punct(Punct::LParen) {
            self.attribute_args()
        } else {
            AttrArgList::EMPTY
        };
        Some(Attribute { namespace, name, args, syntax, span: self.span_from(start) })
    }

    /// An attribute's name, which may be spelled with a keyword.
    ///
    /// `__attribute__((const))` and `[[gnu::const]]` are both written with a keyword, and so are
    /// `noreturn`, `volatile` and several more. An attribute name is not an identifier in the
    /// grammar's sense, it is a token, so a keyword here is the ordinary case rather than an
    /// error to be recovered from.
    fn attribute_name(&mut self) -> Option<Symbol> {
        let token = self.cursor.current();
        if matches!(token.kind, TokenKind::Ident | TokenKind::Keyword(_)) {
            self.cursor.bump();
            return Some(Symbol::from_raw(token.value));
        }
        let found = self.describe(token);
        self.error("E0401", format!("expected an attribute name, found {found}"), token.span);
        None
    }

    /// The parenthesised arguments of an attribute.
    ///
    /// An argument that is a lone identifier stays an identifier rather than becoming a name
    /// expression, because `format(printf, 1, 2)` and `mode(DI)` name things that are not
    /// objects and looking them up in the ordinary scope would find the wrong thing or nothing.
    fn attribute_args(&mut self) -> AttrArgList {
        let mut args = Vec::new();
        if !self.enter() {
            self.cursor.bump();
            return AttrArgList::EMPTY;
        }
        self.cursor.bump();
        while !self.cursor.at_punct(Punct::RParen) && !self.cursor.is_eof() {
            let before = self.cursor.index();
            let lone_ident = self.cursor.current().ident().filter(|_| {
                matches!(self.cursor.peek(1).punct(), Some(Punct::Comma | Punct::RParen))
            });
            match lone_ident {
                Some(name) => {
                    self.cursor.bump();
                    args.push(AttrArg::Ident(name));
                }
                None => args.push(AttrArg::Expr(self.assign_expr())),
            }
            if !self.cursor.eat_punct(Punct::Comma) {
                break;
            }
            if self.cursor.index() == before {
                break;
            }
        }
        self.expect_punct(Punct::RParen);
        self.leave();
        self.ast.add_attr_args(&args)
    }

    /// A `declaration-specifiers`, which is also a `specifier-qualifier-list` since the two
    /// differ only in which specifiers are legal and that is a semantic question.
    pub(crate) fn decl_specs(&mut self) -> DeclSpecsId {
        let start = self.cursor.span();
        self.decl_specs_with(AttrList::EMPTY, start)
    }

    /// A specifier list with the attributes written in front of it already read.
    ///
    /// A declaration may begin with attributes, and whether it does is not known until they have
    /// been read and something has been done with what follows them: `[[x]];` is an attribute
    /// declaration and `[[x]] int a;` is a declaration with attributes on it. The caller reads
    /// them, decides, and hands them back here.
    pub(crate) fn decl_specs_with(&mut self, leading: AttrList, start: Span) -> DeclSpecsId {
        let mut specs = DeclSpecs::empty(start);
        let mut builtin = Builtin::NONE;
        let mut attrs = self.ast[leading].to_vec();
        // Whether something other than the built-in keywords has named a type, which is what
        // stops a second one being read and what stops an identifier being read at all.
        let mut named = false;
        // The `auto` keywords, whose meaning the rest of the list decides.
        let mut autos = Autos { count: 0, span: start };

        loop {
            let token = self.cursor.current();
            let span = token.span;
            match token.kind {
                TokenKind::Punct(Punct::LBracket) if self.at_standard_attribute() => {
                    self.standard_attributes(&mut attrs);
                }
                TokenKind::Ident => {
                    if named || !builtin.is_none() {
                        break;
                    }
                    let name = Symbol::from_raw(token.value);
                    if !self.scopes.is_typedef_name(name) {
                        break;
                    }
                    self.cursor.bump();
                    specs.ty = TypeSpec::Typedef(name);
                    named = true;
                }
                TokenKind::Keyword(word) => {
                    let mut state = Pending {
                        specs: &mut specs,
                        builtin: &mut builtin,
                        named: &mut named,
                        autos: &mut autos,
                    };
                    if !self.decl_spec_keyword(word, span, &mut state) {
                        break;
                    }
                }
                _ => break,
            }
        }

        if !builtin.is_none() {
            specs.ty = TypeSpec::Builtin(builtin);
        }
        self.settle_auto(&mut specs, autos);
        self.settle_constexpr(&specs, start);
        // Joined rather than assigned, because the `__attribute__` arm above puts what it read
        // straight on the specifiers while the `[[...]]` spelling and whatever the caller
        // handed over are collected here, and assigning would drop the first of the two.
        let collected = self.ast.add_attr_list(&attrs);
        specs.attrs = self.join_attrs(collected, specs.attrs);
        specs.span = self.span_from(start);
        self.ast.add_specs(specs)
    }

    /// Which of the two things the `auto` keywords in a finished list were, if there were any.
    ///
    /// C23's `auto` deduces a type and C89's `auto` is a storage class nobody writes, and they
    /// are the same keyword. The rule is the standard's: it is the type specifier when the
    /// declaration names no other type, which is not known until the whole list has been read,
    /// which is why the keyword is counted on the way through rather than written down as one
    /// or the other where it is found. `static auto x = 1;` is both at once and is a
    /// declaration gcc takes.
    ///
    /// A `typedef` is the exception. It names a type rather than deducing one, so `auto` next
    /// to it is the storage class and the two of them are two storage classes, which is what
    /// gcc calls it as well.
    fn settle_auto(&mut self, specs: &mut DeclSpecs, autos: Autos) {
        if autos.count == 0 {
            return;
        }
        if autos.count > 1 {
            self.error("E0406", "duplicate `auto`", autos.span);
        }
        let deduces = self.cx.std >= Std::C23
            && matches!(specs.ty, TypeSpec::None)
            && specs.storage != Some(StorageClass::Typedef);
        if deduces {
            specs.ty = TypeSpec::Auto(Deduction::Auto);
            return;
        }
        if specs.storage.is_some() {
            // The one that was already there is kept, since it is the one the rest of the
            // declaration was written for: `typedef auto T;` still declares a type name.
            self.error("E0404", MULTIPLE_STORAGE_CLASSES, autos.span);
            return;
        }
        specs.storage = Some(StorageClass::Auto);
    }

    /// Whether `constexpr` in a finished list is beside something it may not be beside.
    ///
    /// C23 6.7.1 allows it with `auto`, `register` and `static` and with nothing else. The one
    /// that has to wait for the whole list is `auto`, which is a storage class only where the
    /// declaration named a type some other way: `constexpr auto x = 1;` is one specifier and a
    /// deduced type and is fine, and `auto constexpr int x = 1;` is two storage classes and is
    /// not. gcc names both keywords in each of these and always the same way round whichever
    /// order they were written in, so the wording is fixed per pair rather than built from what
    /// came first.
    fn settle_constexpr(&mut self, specs: &DeclSpecs, start: Span) {
        if !specs.constexpr {
            return;
        }
        let clash = match specs.storage {
            Some(StorageClass::Typedef) => "`constexpr` used with `typedef`",
            Some(StorageClass::Extern) => "`constexpr` used with `extern`",
            Some(StorageClass::Auto) => "`auto` used with `constexpr`",
            _ if specs.thread_local => "`_Thread_local` used with `constexpr`",
            _ => return,
        };
        self.error("E0404", clash, start);
    }

    /// One keyword of a specifier list, and whether it was one at all.
    fn decl_spec_keyword(&mut self, word: Keyword, span: Span, state: &mut Pending<'_>) -> bool {
        let Pending { specs, builtin, named, autos } = state;
        let (specs, builtin, named) = (&mut **specs, &mut **builtin, &mut **named);
        if word == Keyword::Auto {
            // Counted rather than recorded, because what it is depends on the rest of the list.
            self.cursor.bump();
            if autos.count == 0 {
                autos.span = span;
            }
            autos.count += 1;
            return true;
        }
        if let Some(set) = builtin_keyword(word) {
            self.cursor.bump();
            if *named {
                self.two_types(span);
                return true;
            }
            match builtin.add(set) {
                Ok(next) => *builtin = next,
                Err(BuiltinError::Duplicate) => {
                    self.error("E0406", format!("duplicate `{}`", word.as_str()), span);
                }
                Err(BuiltinError::TooManyLongs) => {
                    self.error("E0406", "`long long long` is too long for this compiler", span);
                }
            }
            return true;
        }
        if let Some(qual) = qual_keyword(word) {
            self.cursor.bump();
            specs.quals = specs.quals.with(qual);
            return true;
        }
        if let Some(storage) = storage_keyword(word) {
            self.cursor.bump();
            if let Some(previous) = specs.storage {
                if previous == storage {
                    let message = format!("duplicate `{}`", storage.spelling());
                    self.error("E0406", message, span);
                } else {
                    self.error("E0404", MULTIPLE_STORAGE_CLASSES, span);
                }
            }
            specs.storage = Some(storage);
            return true;
        }
        match word {
            // `__extension__` says the declaration uses a GNU extension deliberately, so that
            // `-pedantic` says nothing about it. It specifies nothing and contributes nothing to
            // the type, and it belongs here rather than in front of the list because that is
            // where glibc writes it: every declaration in its headers that mentions `long long`
            // begins with one, and a parser that stops at it stops at `stdlib.h`. Suppressing the
            // warnings waits on `-pedantic` being wired through, which is the same place the
            // expression form of the keyword is waiting.
            Keyword::Extension => {
                self.cursor.bump();
            }
            Keyword::ThreadLocal => {
                self.cursor.bump();
                specs.thread_local = true;
            }
            // Not one of the storage classes, though C23 counts it among them, because it is
            // one of the two that may be written beside another. Which pairings are allowed is
            // decided once the list has been read, since `auto` is not known to be a storage
            // class until then.
            Keyword::Constexpr => {
                self.cursor.bump();
                if specs.constexpr {
                    self.error("E0406", "duplicate `constexpr`", span);
                }
                specs.constexpr = true;
            }
            Keyword::Inline => {
                self.cursor.bump();
                specs.func = specs.func.with(FuncSpecs::INLINE);
            }
            Keyword::Noreturn => {
                self.cursor.bump();
                specs.func = specs.func.with(FuncSpecs::NORETURN);
            }
            Keyword::Attribute => {
                let mut attrs = Vec::new();
                self.gnu_attributes(&mut attrs);
                let list = self.ast.add_attr_list(&attrs);
                // Attributes in the middle of a specifier list appertain to the declaration, so
                // a second run of them extends the first rather than replacing it. The two runs
                // are adjacent in the table only if nothing was added between them, which is
                // why an already non-empty list is joined here rather than assumed contiguous.
                specs.attrs = self.join_attrs(specs.attrs, list);
            }
            Keyword::Alignas => {
                self.cursor.bump();
                let align = self.align_spec();
                if specs.align.is_none() {
                    specs.align = align;
                }
            }
            Keyword::Struct | Keyword::Union => {
                let kind =
                    if word == Keyword::Struct { RecordKind::Struct } else { RecordKind::Union };
                let ty = self.record(kind);
                self.set_type(specs, builtin, named, ty, span);
            }
            Keyword::Enum => {
                let ty = self.enumeration();
                self.set_type(specs, builtin, named, ty, span);
            }
            Keyword::Typeof | Keyword::TypeofUnqual => {
                let ty = self.typeof_spec(word == Keyword::TypeofUnqual);
                self.set_type(specs, builtin, named, ty, span);
            }
            Keyword::BitInt => {
                // One of the built-in type keywords rather than a specifier of its own, since
                // a sign may be written on either side of it: `unsigned _BitInt(8)` and
                // `_BitInt(8) unsigned` are the same type and neither half names one alone.
                self.cursor.bump();
                let Some(width) = self.bit_int_width() else {
                    return true;
                };
                if *named {
                    self.two_types(span);
                    return true;
                }
                match builtin.add_bit_int(width) {
                    Ok(next) => *builtin = next,
                    // A second one is two types rather than a repeated keyword, since the two
                    // widths need not agree and gcc says the same.
                    Err(_) => self.two_types(span),
                }
            }
            Keyword::AutoType => {
                self.cursor.bump();
                self.set_type(specs, builtin, named, TypeSpec::Auto(Deduction::AutoType), span);
            }
            Keyword::BuiltinVaList => {
                self.cursor.bump();
                self.set_type(specs, builtin, named, TypeSpec::VaList, span);
            }
            Keyword::Atomic => {
                // `_Atomic(T)` builds a type and `_Atomic` on its own qualifies one. They are
                // told apart by what follows, and a `(` that does not start a type name belongs
                // to the declarator, as in `int _Atomic (*p);`.
                let constructor = !*named
                    && builtin.is_none()
                    && self.cursor.peek(1).punct() == Some(Punct::LParen)
                    && self.starts_type_name(self.cursor.peek(2));
                self.cursor.bump();
                if constructor {
                    self.cursor.bump();
                    let ty = self.type_name();
                    self.expect_punct(Punct::RParen);
                    self.set_type(specs, builtin, named, TypeSpec::Atomic(ty), span);
                } else {
                    specs.quals = specs.quals.with(Quals::ATOMIC);
                }
            }
            _ => return false,
        }
        true
    }

    /// Records a type specifier that is not one of the built-in keywords.
    fn set_type(
        &mut self,
        specs: &mut DeclSpecs,
        builtin: &Builtin,
        named: &mut bool,
        ty: TypeSpec,
        span: Span,
    ) {
        if *named || !builtin.is_none() {
            self.two_types(span);
            return;
        }
        specs.ty = ty;
        *named = true;
    }

    /// The message for a declaration that names a type twice, which is GCC's wording.
    fn two_types(&mut self, span: Span) {
        self.error("E0405", "two or more data types in declaration specifiers", span);
    }

    /// Two attribute lists as one, copying only when both have something in them.
    fn join_attrs(&mut self, first: AttrList, second: AttrList) -> AttrList {
        if first.is_empty() {
            return second;
        }
        if second.is_empty() {
            return first;
        }
        let mut both: Vec<_> = self.ast[first].to_vec();
        both.extend_from_slice(&self.ast[second]);
        self.ast.add_attr_list(&both)
    }

    /// `alignas ( type-name )` or `alignas ( constant-expression )`.
    fn align_spec(&mut self) -> Option<AlignSpec> {
        if !self.expect_punct(Punct::LParen) {
            return None;
        }
        let align = if self.starts_type_name(self.cursor.current()) {
            AlignSpec::Type(self.type_name())
        } else {
            AlignSpec::Expr(self.const_expr())
        };
        self.expect_punct(Punct::RParen);
        Some(align)
    }

    /// `typeof ( expression )` or `typeof ( type-name )`.
    fn typeof_spec(&mut self, unqual: bool) -> TypeSpec {
        self.cursor.bump();
        if !self.expect_punct(Punct::LParen) {
            return TypeSpec::None;
        }
        let operand = if self.starts_type_name(self.cursor.current()) {
            TypeofArg::Type(self.type_name())
        } else {
            TypeofArg::Expr(self.expr())
        };
        self.expect_punct(Punct::RParen);
        TypeSpec::Typeof { unqual, operand }
    }

    /// The `( constant-expression )` of a `_BitInt`, with the keyword already read.
    fn bit_int_width(&mut self) -> Option<ExprId> {
        if !self.expect_punct(Punct::LParen) {
            return None;
        }
        let width = self.const_expr();
        self.expect_punct(Punct::RParen);
        Some(width)
    }

    /// A `struct` or `union` specifier, with or without a tag and with or without a body.
    fn record(&mut self, kind: RecordKind) -> TypeSpec {
        self.cursor.bump();
        let mut attrs = Vec::new();
        self.collect_attributes(&mut attrs);
        let tag = self.cursor.current().ident();
        let tag_span = self.cursor.span();
        if tag.is_some() {
            self.cursor.bump();
        }
        let fields = if self.cursor.at_punct(Punct::LBrace) {
            if let Some(name) = tag {
                let tag_kind =
                    if kind == RecordKind::Struct { TagKind::Struct } else { TagKind::Union };
                self.scopes.declare_tag(name, tag_kind);
            }
            Some(self.members())
        } else {
            if tag.is_none() {
                let found = self.describe(self.cursor.current());
                let message = format!("expected a tag or a body after `{}`, found {found}", {
                    kind.spelling()
                });
                self.error("E0407", message, tag_span);
            }
            None
        };
        // Read here because the closing brace is where the `#pragma pack` that applies is the
        // one in effect, and read even when there is no body, since reading a line is also what
        // complains about a malformed one and those complaints belong in source order.
        let in_effect = self.pack_in_effect();
        let pack = if fields.is_some() { in_effect } else { None };
        // GCC takes attributes after the closing brace as well, which is where `packed` is
        // usually written, and they appertain to the same tag as the ones before it.
        self.collect_attributes(&mut attrs);
        let attrs = self.ast.add_attr_list(&attrs);
        TypeSpec::Record { kind, tag, fields, attrs, pack }
    }

    /// The `{ ... }` of a struct or union.
    fn members(&mut self) -> MemberList {
        let mut members = Vec::new();
        if !self.enter() {
            self.cursor.bump();
            return MemberList::EMPTY;
        }
        self.cursor.bump();
        while !self.cursor.at_punct(Punct::RBrace) && !self.cursor.is_eof() && !self.stopped() {
            let before = self.cursor.index();
            self.member(&mut members);
            if self.cursor.index() == before {
                // Nothing was consumed, so the token starts no member and the list would spin
                // on it. Report it once and step over it.
                let found = self.describe(self.cursor.current());
                let span = self.cursor.span();
                self.error("E0407", format!("expected a member, found {found}"), span);
                self.cursor.bump();
            }
        }
        self.expect_punct(Punct::RBrace);
        self.leave();
        self.ast.add_member_list(&members)
    }

    /// One member declaration, which may declare several members.
    fn member(&mut self, out: &mut Vec<Member>) {
        let start = self.cursor.span();
        if self.cursor.at_keyword(Keyword::StaticAssert) {
            let (cond, message) = self.static_assert_body();
            self.expect_punct(Punct::Semi);
            out.push(Member::StaticAssert { cond, message, span: self.span_from(start) });
            return;
        }
        // A stray semicolon in a member list is an extension GCC accepts and warns about only
        // under `-pedantic`, and real headers built by macro have them.
        if self.cursor.eat_punct(Punct::Semi) {
            self.pedantic("E0408", "extra `;` in a member list", start);
            return;
        }
        let specs = self.decl_specs();
        if self.cursor.eat_punct(Punct::Semi) {
            // An anonymous struct or union member, which is C11 and was GNU long before, or a
            // tag declared inside another one. Either way there is no declarator.
            let attrs = self.ast[specs].attrs;
            out.push(Member::Field(Field {
                specs,
                declarator: None,
                bits: None,
                attrs,
                span: self.span_from(start),
            }));
            return;
        }
        loop {
            let at = self.cursor.span();
            let before = self.cursor.index();
            let declarator =
                if self.cursor.at_punct(Punct::Colon) { None } else { Some(self.declarator()) };
            let bits =
                if self.cursor.eat_punct(Punct::Colon) { Some(self.const_expr()) } else { None };
            let attrs = self.attributes();
            out.push(Member::Field(Field {
                specs,
                declarator,
                bits,
                attrs,
                span: self.span_from(at),
            }));
            if !self.cursor.eat_punct(Punct::Comma) {
                break;
            }
            if self.cursor.index() == before {
                break;
            }
        }
        self.expect_punct(Punct::Semi);
    }

    /// An `enum` specifier.
    fn enumeration(&mut self) -> TypeSpec {
        self.cursor.bump();
        let mut attrs = Vec::new();
        self.collect_attributes(&mut attrs);
        let tag = self.cursor.current().ident();
        let tag_span = self.cursor.span();
        if tag.is_some() {
            self.cursor.bump();
        }
        // C23's fixed underlying type. The colon is also how a bit-field is written, so
        // `struct { enum E : 3; }` has to keep meaning a three-bit field of type `enum E`, and
        // what tells them apart is whether a type name follows.
        let underlying =
            if self.cursor.at_punct(Punct::Colon) && self.starts_type_name(self.cursor.peek(1)) {
                self.cursor.bump();
                Some(self.type_name())
            } else {
                None
            };
        let enumerators = if self.cursor.at_punct(Punct::LBrace) {
            if let Some(name) = tag {
                self.scopes.declare_tag(name, TagKind::Enum);
            }
            Some(self.enumerators())
        } else {
            if tag.is_none() {
                let found = self.describe(self.cursor.current());
                let message = format!("expected a tag or a body after `enum`, found {found}");
                self.error("E0407", message, tag_span);
            }
            None
        };
        self.collect_attributes(&mut attrs);
        let attrs = self.ast.add_attr_list(&attrs);
        TypeSpec::Enum { tag, enumerators, underlying, attrs }
    }

    /// The `{ ... }` of an enumeration.
    fn enumerators(&mut self) -> EnumeratorList {
        let mut out = Vec::new();
        if !self.enter() {
            self.cursor.bump();
            return EnumeratorList::EMPTY;
        }
        self.cursor.bump();
        while !self.cursor.at_punct(Punct::RBrace) && !self.cursor.is_eof() {
            let start = self.cursor.span();
            let before = self.cursor.index();
            let Some((name, _)) = self.expect_ident() else { break };
            // An enumerator is an ordinary identifier, not a tag, so `enum E { T };` after
            // `typedef int T;` shadows the type name from here to the end of the scope.
            self.scopes.declare(name, IdentKind::Ordinary);
            let attrs = self.attributes();
            let value =
                if self.cursor.eat_punct(Punct::Eq) { Some(self.const_expr()) } else { None };
            out.push(Enumerator { name, value, attrs, span: self.span_from(start) });
            if !self.cursor.eat_punct(Punct::Comma) {
                break;
            }
            if self.cursor.index() == before {
                break;
            }
        }
        self.expect_punct(Punct::RBrace);
        self.leave();
        self.ast.add_enumerator_list(&out)
    }

    /// The `( cond )` or `( cond, "message" )` of a static assertion, keyword included.
    pub(crate) fn static_assert_body(&mut self) -> (ExprId, Option<StrId>) {
        let start = self.cursor.span();
        self.cursor.bump();
        if !self.expect_punct(Punct::LParen) {
            return (self.poison_expr(start), None);
        }
        let cond = self.const_expr();
        let mut message = None;
        if self.cursor.eat_punct(Punct::Comma) {
            message = self.string_literal();
        }
        self.expect_punct(Punct::RParen);
        (cond, message)
    }
}
