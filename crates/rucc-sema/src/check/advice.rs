//! The attributes that exist to have something said at a use: `deprecated`, `warn_unused_result`
//! and C23's `[[nodiscard]]`.
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
//! What is not here yet: `deprecated` on a type, a tag, an enumerator or a member, all of which gcc
//! also reports a use of. A program using one of those compiles the same and is not told.

use rucc_ast::{AttrArg, AttrList, AttrSyntax, Attribute};
use rucc_base::hash::{Map, Set};
use rucc_diag::{Diagnostic, Span};
use rucc_gnu::{Kind, Status};
use rucc_lex::Encoding;
use rucc_types::is_void;

use crate::check::Checker;
use crate::decl::DeclId;
use crate::expr::{Conversion, ExprId, ExprKind};
use crate::stmt::Stmt;

/// The code of the `deprecated` warning, which answers to `-Wdeprecated-declarations`.
pub(in crate::check) const DEPRECATED: &str = "E0770";

/// The code of the two unused result warnings, which answer to `-Wunused-result`.
pub(in crate::check) const UNUSED_RESULT: &str = "E0771";

/// What the declarations of a name asked to have said about a use of it.
#[derive(Debug, Default)]
pub(in crate::check) struct Advice {
    /// The names some declaration marked `deprecated`, with the message the last one to give a
    /// message gave.
    deprecated: Map<DeclId, Option<String>>,
    /// The functions some declaration marked `warn_unused_result`.
    unused_result: Set<DeclId>,
    /// The functions some declaration marked `[[nodiscard]]`, with its message.
    nodiscard: Map<DeclId, Option<String>>,
}

/// The one attribute of the three an attribute is, if it is one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Deprecated,
    UnusedResult,
    Nodiscard,
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
                    Which::Deprecated => {
                        let message = self.advice_message(attr);
                        let kept = self.advice.deprecated.entry(decl).or_default();
                        if message.is_some() || kept.is_none() {
                            *kept = message;
                        }
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
                }
            }
        }
    }

    /// Which of the three an attribute is, read through the matrix so that nothing is said for a
    /// row `__has_attribute` has told the program is not done.
    ///
    /// `deprecated` is both GCC's attribute and the standard's, so it is read in either spelling.
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
            "warn_unused_result" if gnu => Which::UnusedResult,
            "nodiscard" if standard => Which::Nodiscard,
            _ => return None,
        };
        let row = rucc_gnu::lookup(kind, name)?;
        matches!(row.status, Status::Implemented | Status::Partial).then_some(which)
    }

    /// The message `deprecated("...")` or `[[nodiscard("...")]]` was given, if it was given one
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

    /// Says that a name marked `deprecated` was used, where it was used.
    pub(in crate::check) fn heed_deprecated(&mut self, decl: DeclId, span: Span) {
        let Some(message) = self.advice.deprecated.get(&decl).cloned() else { return };
        let Some(name) = self.tast[decl].name else { return };
        let name = self.text(name).to_owned();
        let what = match message {
            Some(message) => format!("'{name}' is deprecated: {message}"),
            None => format!("'{name}' is deprecated"),
        };
        let at = self.tast.decl_span(decl);
        self.report(
            Diagnostic::warning(what, span).with_code(DEPRECATED).note("declared here", at),
        );
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
