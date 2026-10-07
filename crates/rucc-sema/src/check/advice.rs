//! The attributes that exist to have something said at a use: `deprecated`, `warn_unused_result`,
//! C23's `[[nodiscard]]`, `sentinel` and `designated_init`, and the reading of `format` and
//! `format_arg`, whose checks are in `check/format.rs`.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4.
//!
//! None of the three changes a byte of what the program compiles to. What each asks for is a
//! warning, and the warning is the whole of the attribute: a library marks the function it is
//! about to take away `deprecated` so that the programs still calling it hear about it, and marks
//! `read` or `realloc` `warn_unused_result` so that a program throwing away the one answer that
//! says whether it worked hears about that. A compiler that takes the attribute and says nothing
//! compiles every such program correctly and tells none of them what the library wanted them told,
//! which is why `__has_attribute` only answers yes for these because the warnings are here.
//!
//! What is said and where is gcc 16's, measured one case at a time:
//!
//! * `deprecated` is said at every use of the name in an expression, a call inside the function's
//!   own body included, with the message when one was given and a note at the declaration.
//! * `deprecated` on a typedef, a tag, a member or an enumerator is said at a use of that too: a
//!   typedef name in a type, a tag written without a body, a member reached with `.` or `->`, and
//!   an enumerator in an expression. Defining the tag is not a use and neither is `sizeof` of an
//!   object of the type. A typedef gets no note, as in gcc.
//! * `unavailable`, from gcc 12, is `deprecated` made an error: said at the same uses, in the
//!   same words with `unavailable` for `deprecated`, and with no option that turns it off. It
//!   outranks `deprecated` whichever of the two came first, on one declaration or across several,
//!   and the message said is then only ever one `unavailable` gave.
//! * `warn_unused_result` is said of a call whose value an expression statement, the step of a
//!   `for` or the left side of a comma throws away, and a cast to `void` does not keep it quiet,
//!   since the attribute is for the answer a program must not ignore even on purpose. gcc says
//!   this one late, once the function has been read, and so does this.
//! * `[[nodiscard]]` is said in the same places, except under a cast to `void`, which is how the
//!   standard's attribute is meant to be answered, with the message quoted when one was given.
//!
//! The value a statement expression hands back is its last statement's, so that statement throws
//! nothing away unless the statement expression itself is thrown away. That is why the calls are
//! kept until the body is finished rather than reported where the statement is read.
//!
//! * `sentinel` is said of a call to a variadic function whose last argument, or the one
//!   `sentinel(N)` places before it, is not a null pointer: `0` is not one, because it is an
//!   `int` and a variadic function reading a pointer reads more than was passed where the two are
//!   different widths. `NULL` and `(char *)0` are. gcc files it under `-Wformat`, so it is off
//!   until that or `-Wall` asks for it, and gcc's own `execl`, `execlp` and `execle` have it
//!   without the attribute.
//! * `designated_init` on a structure is said of each value an initializer list gives one of its
//!   members by position, at the value, which includes `{0}` and not `{}`. It is on by default.

use rucc_ast::{AttrArg, AttrList, AttrSyntax, Attribute};
use rucc_base::Symbol;
use rucc_base::hash::{Map, Set};
use rucc_diag::{Diagnostic, Span};
use rucc_gnu::{Kind, Status};
use rucc_lex::Encoding;
use rucc_types::{EnumId, FunctionType, RecordId, TypeId, TypeKind, is_void};

use crate::check::Checker;
use crate::check::format::Format;
use crate::check::nonnull::Nonnull;
use crate::decl::DeclId;
use crate::expr::{Conversion, ExprId, ExprKind};
use crate::stmt::Stmt;

/// The code of the `deprecated` warning, which answers to `-Wdeprecated-declarations`.
pub(in crate::check) const DEPRECATED: &str = "E0770";

/// The code of the `unavailable` error.
const UNAVAILABLE: &str = "E0815";

/// The code of the two unused result warnings, which answer to `-Wunused-result`.
pub(in crate::check) const UNUSED_RESULT: &str = "E0771";

/// The code of the two sentinel warnings, which gcc files under `-Wformat`.
const SENTINEL: &str = "E0789";

/// The code of the `designated_init` warning, which answers to `-Wdesignated-init`.
const DESIGNATED_INIT: &str = "E0790";

/// What the declarations of a name asked to have said about a use of it.
#[derive(Debug, Default)]
pub(in crate::check) struct Advice {
    /// The names some declaration marked `deprecated` or `unavailable`, with what is to be said.
    deprecated: Map<DeclId, Notice>,
    /// The functions some declaration marked `warn_unused_result`.
    unused_result: Set<DeclId>,
    /// The functions some declaration marked `[[nodiscard]]`, with its message.
    nodiscard: Map<DeclId, Option<String>>,
    /// The functions some declaration marked `format`, with what the last one said.
    pub(in crate::check) format: Map<DeclId, Format>,
    /// The functions some declaration marked `format_arg`, with the parameter it names.
    pub(in crate::check) format_arg: Map<DeclId, usize>,
    /// The functions some declaration marked `sentinel`, with how far from the end it is.
    sentinel: Map<DeclId, usize>,
    /// The functions some declaration marked `nonnull` or `nonnull_if_nonzero`, with which
    /// arguments, for `check/nonnull.rs`.
    pub(in crate::check) nonnull: Map<DeclId, Nonnull>,
    /// The structures marked `designated_init`.
    designated: Set<RecordId>,
    /// Typedef names marked `deprecated`, by the name and the type. A plain typedef is bound to
    /// the type it names rather than to a type of its own, so the type alone would mark `int`
    /// along with it.
    typedefs: Map<(Symbol, TypeId), Notice>,
    /// Structures, unions and enumerations marked `deprecated`, by the type their tag names.
    tags: Map<TypeId, Marked>,
    /// Members marked `deprecated`, by the record they are directly in and their name.
    members: Map<(RecordId, Symbol), Marked>,
    /// Enumerators marked `deprecated`, by name, value and type, which is everything a use of one
    /// resolves to.
    enumerators: Map<(Symbol, i128, TypeId), Marked>,
    /// Enumerators marked `unused` or `[[maybe_unused]]`, by their enumeration and name, which a
    /// `switch` that leaves them out is quiet about, for `check/switch.rs`.
    pub(in crate::check) unused_enumerators: Set<(EnumId, Symbol)>,
    /// Enumerations marked `flag_enum`, whose values a `switch` may combine, for
    /// `check/switch.rs`.
    pub(in crate::check) flag_enums: Set<EnumId>,
}

/// What `deprecated` and `unavailable` asked to have said at a use of a name.
#[derive(Debug, Clone, Default)]
struct Notice {
    /// The message to say after the name, if one was given.
    message: Option<String>,
    /// Whether `unavailable` was written, which makes a use an error.
    unavailable: bool,
}

impl Notice {
    /// Takes one more `deprecated` or `unavailable` into what is to be said. `unavailable`
    /// outranks `deprecated` and drops the message that one gave, and between two of the same
    /// rank the last message given is the one kept, which is gcc's rule.
    fn hear(&mut self, message: Option<String>, unavailable: bool) {
        if unavailable && !self.unavailable {
            *self = Self { message, unavailable };
        } else if unavailable == self.unavailable && message.is_some() {
            self.message = message;
        }
    }
}

/// What `deprecated` said about a name that has no declaration of its own to point the note at.
#[derive(Debug, Clone)]
struct Marked {
    /// What is to be said.
    notice: Notice,
    /// Where the name was declared, for the note.
    at: Span,
}

/// The one attribute of the three an attribute is, if it is one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Deprecated,
    Unavailable,
    UnusedResult,
    Nodiscard,
    Format,
    FormatArg,
    Sentinel,
}

impl Checker<'_> {
    /// Reads what a declaration's attribute lists ask to have said at a use of it.
    ///
    /// Called with the same lists the annotations are kept from, and after the declaration has
    /// been merged with the ones above it, so `decl` is the name and not this declaration of it.
    /// One declaration asking is enough, which is gcc's rule: the header says it and the file that
    /// defines the function does not repeat it.
    pub(in crate::check) fn read_advice(&mut self, decl: DeclId, lists: &[AttrList]) {
        for &list in lists {
            let written = self.ast[list].to_vec();
            for attr in written {
                let Some(which) = self.which(&attr) else { continue };
                match which {
                    Which::Deprecated | Which::Unavailable => {
                        let message = self.advice_message(attr);
                        let unavailable = which == Which::Unavailable;
                        self.advice.deprecated.entry(decl).or_default().hear(message, unavailable);
                    }
                    Which::UnusedResult => {
                        self.advice.unused_result.insert(decl);
                    }
                    Which::Nodiscard => {
                        let message = self.advice_message(attr);
                        let kept = self.advice.nodiscard.entry(decl).or_default();
                        if message.is_some() || kept.is_none() {
                            *kept = message;
                        }
                    }
                    Which::Format => {
                        if let Some(format) = self.format_attribute(attr) {
                            self.advice.format.insert(decl, format);
                        }
                    }
                    Which::FormatArg => {
                        let args = self.ast[attr.args].to_vec();
                        let number = match args.as_slice() {
                            [arg] => self.attribute_number(*arg),
                            _ => None,
                        };
                        if let Some(number) = number {
                            self.advice.format_arg.insert(decl, number);
                        }
                    }
                    Which::Sentinel => {
                        let args = self.ast[attr.args].to_vec();
                        let position = match args.as_slice() {
                            [] => Some(0),
                            [arg] => self.attribute_number(*arg),
                            _ => None,
                        };
                        if let Some(position) = position {
                            self.advice.sentinel.insert(decl, position);
                        }
                    }
                }
            }
        }
    }

    /// Which of the three an attribute is, read through the matrix so that nothing is said for a
    /// row `__has_attribute` has told the program is not done.
    ///
    /// `deprecated` is both GCC's attribute and the standard's, so it is read in either spelling.
    /// `unavailable` is only GCC's, and only from gcc 12, so a persona claiming an older one has
    /// it said as unknown and not read, as [`Self::gnu_name`] has it.
    /// `nodiscard` is only the standard's, and gcc does not know `__attribute__((nodiscard))` in C,
    /// so it is read only as `[[nodiscard]]`. `warn_unused_result` is only GCC's.
    fn which(&self, attr: &Attribute) -> Option<Which> {
        let name = rucc_gnu::unarmour(self.text(attr.name));
        let standard = attr.syntax == AttrSyntax::Standard && attr.namespace.is_none();
        let gnu = match attr.namespace {
            Some(ns) => self.text(ns) == "gnu",
            None => attr.syntax != AttrSyntax::Standard,
        };
        let kind = if standard { Kind::CAttribute } else { Kind::Attribute };
        if !standard && !gnu {
            return None;
        }
        let which = match name {
            "deprecated" => Which::Deprecated,
            "unavailable" if gnu => Which::Unavailable,
            "warn_unused_result" if gnu => Which::UnusedResult,
            "nodiscard" if standard => Which::Nodiscard,
            "format" if gnu => Which::Format,
            "format_arg" if gnu => Which::FormatArg,
            "sentinel" if gnu => Which::Sentinel,
            _ => return None,
        };
        let row = rucc_gnu::lookup(kind, name)?;
        let read = matches!(row.status, Status::Implemented | Status::Partial);
        (read && (standard || row.is_known_to(self.cx.gnuc))).then_some(which)
    }

    /// Whether an attribute list on a structure says `designated_init`, read through the matrix
    /// the way [`Self::which`] reads the others.
    pub(in crate::check) fn designated_init(&self, attrs: AttrList) -> bool {
        self.ast[attrs].iter().any(|attr| {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                return false;
            }
            if attr.syntax == AttrSyntax::Standard && attr.namespace.is_none() {
                return false;
            }
            let name = rucc_gnu::unarmour(self.text(attr.name));
            name == "designated_init"
                && rucc_gnu::lookup(Kind::Attribute, name)
                    .is_some_and(|row| matches!(row.status, Status::Implemented | Status::Partial))
        })
    }

    /// Keeps that a structure was marked `designated_init`.
    pub(in crate::check) fn mark_designated(&mut self, record: RecordId) {
        self.advice.designated.insert(record);
    }

    /// Says that an initializer list gave a member of a `designated_init` structure its value by
    /// position, at the value.
    pub(in crate::check) fn heed_designated(&mut self, record: RecordId, span: Span) {
        if !self.advice.designated.contains(&record) {
            return;
        }
        let what = "positional initialization of field in 'struct' declared with \
                    'designated_init' attribute";
        self.report(Diagnostic::warning(what, span).with_code(DESIGNATED_INIT));
    }

    /// Says that a call to a function marked `sentinel` does not end where the attribute says
    /// it must, in a null pointer.
    pub(in crate::check) fn heed_sentinel(
        &mut self,
        callee: ExprId,
        signature: &FunctionType,
        args: &[ExprId],
        span: Span,
    ) {
        if !signature.variadic || !signature.prototyped {
            return;
        }
        let Some(decl) = self.called_decl(callee) else { return };
        let position = match self.advice.sentinel.get(&decl) {
            Some(&position) => position,
            None => match self.library_function(decl) {
                Some("execl" | "execlp") => 0,
                Some("execle") => 1,
                _ => return,
            },
        };
        let named = signature.params.len();
        if args.len() < named + position + 1 {
            let what = "not enough variable arguments to fit a sentinel";
            self.report(Diagnostic::warning(what, span).with_code(SENTINEL));
            return;
        }
        let last = args[args.len() - 1 - position];
        if self.is_poisoned(last) {
            return;
        }
        let ty = self.types.canonical(self.tast[last].ty);
        let pointer = matches!(self.types.kind(ty), TypeKind::Pointer(_));
        if !pointer || !self.conv().is_null_pointer_constant(last) {
            let what = "missing sentinel in function call";
            self.report(Diagnostic::warning(what, span).with_code(SENTINEL));
        }
    }

    /// The message `deprecated("...")`, `unavailable("...")` or `[[nodiscard("...")]]` was given, if it was given one
    /// that is a plain string.
    ///
    /// gcc refuses anything else as the argument, and the refusal is not this module's: what is
    /// said here is only ever the warning, so an argument that is not a string leaves the warning
    /// without a message rather than without the warning.
    fn advice_message(&mut self, attr: Attribute) -> Option<String> {
        let args = self.ast[attr.args].to_vec();
        let [AttrArg::Expr(expr)] = args.as_slice() else { return None };
        let checked = self.expr(*expr);
        let ExprKind::Str(id) = self.tast[checked].kind else { return None };
        let literal = &self.tast[id];
        if literal.encoding != Encoding::Plain {
            return None;
        }
        let text: String =
            literal.elements.iter().filter_map(|&unit| char::from_u32(unit)).collect();
        Some(text.trim_end_matches('\0').to_owned())
    }

    /// Says that a name marked `deprecated` or `unavailable` was used, where it was used.
    pub(in crate::check) fn heed_deprecated(&mut self, decl: DeclId, span: Span) {
        let Some(notice) = self.advice.deprecated.get(&decl).cloned() else { return };
        let Some(name) = self.tast[decl].name else { return };
        let at = self.tast.decl_span(decl);
        self.say_deprecated(name, notice, span, Some(at));
    }

    /// What `deprecated` and `unavailable` asked of a name that is not a declaration, and nothing
    /// when neither was written, kept by the rule a declaration's are.
    fn deprecation(&mut self, lists: &[AttrList]) -> Option<Notice> {
        let mut found: Option<Notice> = None;
        for &list in lists {
            let written = self.ast[list].to_vec();
            for attr in written {
                let unavailable = match self.which(&attr) {
                    Some(Which::Deprecated) => false,
                    Some(Which::Unavailable) => true,
                    _ => continue,
                };
                let message = self.advice_message(attr);
                found.get_or_insert_default().hear(message, unavailable);
            }
        }
        found
    }

    /// Reads `deprecated` off a typedef.
    pub(in crate::check) fn read_deprecated_typedef(
        &mut self,
        name: Symbol,
        ty: TypeId,
        lists: &[AttrList],
    ) {
        if let Some(notice) = self.deprecation(lists) {
            self.advice.typedefs.insert((name, ty), notice);
        }
    }

    /// Reads `deprecated` off the list a structure, union or enumeration was defined with.
    pub(in crate::check) fn read_deprecated_tag(&mut self, ty: TypeId, attrs: AttrList, at: Span) {
        if let Some(notice) = self.deprecation(&[attrs]) {
            self.advice.tags.insert(ty, Marked { notice, at });
        }
    }

    /// Reads `deprecated` off a member, from its own list and its specifiers'.
    pub(in crate::check) fn read_deprecated_member(
        &mut self,
        record: RecordId,
        name: Symbol,
        lists: &[AttrList],
        at: Span,
    ) {
        if let Some(notice) = self.deprecation(lists) {
            self.advice.members.insert((record, name), Marked { notice, at });
        }
    }

    /// Reads `deprecated` off an enumerator, once its type is settled.
    pub(in crate::check) fn read_deprecated_enumerator(
        &mut self,
        name: Symbol,
        value: i128,
        ty: TypeId,
        attrs: AttrList,
        at: Span,
    ) {
        if let Some(notice) = self.deprecation(&[attrs]) {
            self.advice.enumerators.insert((name, value, ty), Marked { notice, at });
        }
    }

    /// Says that a typedef name marked `deprecated` was used, which gcc gives no note.
    pub(in crate::check) fn heed_deprecated_typedef(
        &mut self,
        name: Symbol,
        ty: TypeId,
        span: Span,
    ) {
        if let Some(notice) = self.advice.typedefs.get(&(name, ty)).cloned() {
            self.say_deprecated(name, notice, span, None);
        }
    }

    /// Says that a tag marked `deprecated` was written without a body.
    pub(in crate::check) fn heed_deprecated_tag(
        &mut self,
        name: Option<Symbol>,
        ty: TypeId,
        span: Span,
    ) {
        let Some(name) = name else { return };
        if let Some(Marked { notice, at }) = self.advice.tags.get(&ty).cloned() {
            self.say_deprecated(name, notice, span, Some(at));
        }
    }

    /// Says that a member marked `deprecated` was reached, by the record it is directly in.
    pub(in crate::check) fn heed_deprecated_member(
        &mut self,
        record: RecordId,
        name: Symbol,
        span: Span,
    ) {
        if let Some(Marked { notice, at }) = self.advice.members.get(&(record, name)).cloned() {
            self.say_deprecated(name, notice, span, Some(at));
        }
    }

    /// Says that an enumerator marked `deprecated` was used.
    pub(in crate::check) fn heed_deprecated_enumerator(
        &mut self,
        name: Symbol,
        value: i128,
        ty: TypeId,
        span: Span,
    ) {
        let key = (name, value, ty);
        if let Some(Marked { notice, at }) = self.advice.enumerators.get(&key).cloned() {
            self.say_deprecated(name, notice, span, Some(at));
        }
    }

    /// The warning itself, or the error for `unavailable`, in gcc's words, with the note where
    /// there is one to give.
    fn say_deprecated(&mut self, name: Symbol, notice: Notice, span: Span, declared: Option<Span>) {
        let name = self.text(name).to_owned();
        let state = if notice.unavailable { "unavailable" } else { "deprecated" };
        let what = match notice.message {
            Some(message) => format!("'{name}' is {state}: {message}"),
            None => format!("'{name}' is {state}"),
        };
        let mut said = if notice.unavailable {
            Diagnostic::error(what, span).with_code(UNAVAILABLE)
        } else {
            Diagnostic::warning(what, span).with_code(DEPRECATED)
        };
        if let Some(at) = declared {
            said = said.note("declared here", at);
        }
        self.report(said);
    }

    /// Keeps a value a statement throws away, to be looked at once the body is finished.
    pub(in crate::check) fn discard(&mut self, value: ExprId) {
        if let Some(body) = self.body.as_mut() {
            body.discarded.push(value);
        }
    }

    /// Takes back a value [`Self::discard`] kept, because it turned out to be the value of a
    /// statement expression and is not thrown away after all, or not yet.
    pub(in crate::check) fn undiscard(&mut self, value: ExprId) {
        if let Some(body) = self.body.as_mut() {
            if let Some(at) = body.discarded.iter().rposition(|&kept| kept == value) {
                body.discarded.remove(at);
            }
        }
    }

    /// Says what there is to say about each value the body threw away, in the order the
    /// statements were read.
    pub(in crate::check) fn heed_discarded(&mut self, discarded: Vec<ExprId>) {
        for value in discarded {
            self.thrown_away(value, false);
        }
    }

    /// Looks through what a thrown away value was made of for a call whose answer mattered.
    ///
    /// `voided` is whether a cast to `void` has been passed on the way down, which keeps
    /// `[[nodiscard]]` quiet and not `warn_unused_result`. A comma throws its left side away
    /// whatever happens to its right, and any other conversion changes nothing about what was
    /// thrown away.
    fn thrown_away(&mut self, value: ExprId, voided: bool) {
        let node = self.tast[value];
        match node.kind {
            ExprKind::Call { callee, .. } => {
                let span = self.tast.expr_span(value);
                self.ignored_result(callee, voided, span);
            }
            ExprKind::Cast(operand) => {
                let void = is_void(&self.types, self.types.canonical(node.ty));
                self.thrown_away(operand, voided || void);
            }
            // A cast to `void` is written as the conversion to `void` and not as a cast, since the
            // tree already has a node for a value being discarded.
            ExprKind::Convert { kind: Conversion::Void, operand } => {
                self.thrown_away(operand, true)
            }
            ExprKind::Convert { operand, .. } => self.thrown_away(operand, voided),
            ExprKind::Comma { lhs, rhs } => {
                self.thrown_away(lhs, false);
                self.thrown_away(rhs, voided);
            }
            // Only a statement expression that is the one statement, which is where gcc 14 still
            // sees the call. With anything in front of it gcc says nothing, and the kernel's
            // `drmm_mutex_init` is a `mutex_init` and then a call whose answer it leaves unread.
            ExprKind::StmtExpr(stmt) => {
                let alone =
                    matches!(self.tast[stmt], Stmt::Block(body) if self.tast[body].len() == 1);
                if let Some(last) = self.last_value(stmt).filter(|_| alone) {
                    self.thrown_away(last, voided);
                }
            }
            _ => {}
        }
    }

    /// The value of a statement expression's last statement, under whatever labels it carries.
    pub(in crate::check) fn last_value(&self, stmt: crate::stmt::StmtId) -> Option<ExprId> {
        let mut at = match self.tast[stmt] {
            Stmt::Block(body) => *self.tast[body].last()?,
            _ => return None,
        };
        loop {
            match self.tast[at] {
                Stmt::Expr(value) => return Some(value),
                Stmt::Label { body, .. } => at = body,
                _ => return None,
            }
        }
    }

    /// Says that the answer of a call to a function that asked for it to be used was not.
    fn ignored_result(&mut self, callee: ExprId, voided: bool, span: Span) {
        let mut callee = callee;
        while let ExprKind::Convert { operand, .. } = self.tast[callee].kind {
            callee = operand;
        }
        let ExprKind::Decl(decl) = self.tast[callee].kind else { return };
        let Some(name) = self.tast[decl].name else { return };
        let name = self.text(name).to_owned();
        if !voided {
            if let Some(message) = self.advice.nodiscard.get(&decl).cloned() {
                let what = match message {
                    Some(message) => format!(
                        "ignoring return value of '{name}', declared with attribute 'nodiscard': \
                         \"{message}\""
                    ),
                    None => {
                        format!(
                            "ignoring return value of '{name}', declared with attribute 'nodiscard'"
                        )
                    }
                };
                let at = self.tast.decl_span(decl);
                let said = Diagnostic::warning(what, span).with_code(UNUSED_RESULT);
                self.report(said.note("declared here", at));
                return;
            }
        }
        if self.advice.unused_result.contains(&decl) {
            let what = format!(
                "ignoring return value of '{name}' declared with attribute 'warn_unused_result'"
            );
            self.report(Diagnostic::warning(what, span).with_code(UNUSED_RESULT));
        }
    }
}
