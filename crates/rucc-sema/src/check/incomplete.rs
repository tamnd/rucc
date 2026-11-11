//! kencc's `#pragma incomplete`, and the type signatures that `-T` puts in the object.
//!
//! `pragincomplete` in `cc/dpchk.c` sets `GINCOMPLETE` on a structure or a union. The signature
//! of a marked record is the hash of its kind alone, so a header can name a structure whose
//! members are private, and two programs that see different members still agree. The mark is set
//! when kencc reads the line and it stays until the end of the file. So a `signof` before the line
//! sees the members and a `signof` after it does not, and the signatures in the object, which
//! `outcode` writes at the end of the file, see every mark.
//!
//! The parser keeps the lines with their places, and the checker marks each record before it
//! checks the first top-level declaration that comes after the line.

use rucc_ast::RecordKind as AstRecordKind;
use rucc_base::Symbol;
use rucc_base::hash::Map;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{KENCC_SIGN_INTERN, TypeKind, kencc_signature};

use super::Checker;
use super::ty::TagUse;
use crate::decl::Linkage;
use crate::scope::{Binding, TagKind};

/// One `#pragma incomplete` line, as the parser read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncompleteLine {
    /// `struct` or `union` when the line names a tag, and nothing when it names an ordinary
    /// name whose type is the record.
    pub kind: Option<AstRecordKind>,
    /// The name.
    pub name: Symbol,
    /// Where the line is.
    pub span: Span,
}

impl Checker<'_> {
    /// Gives the checker the `#pragma incomplete` lines of the unit, in the order they were
    /// written. Call this before [`Checker::check_unit`].
    pub fn set_incomplete_lines(&mut self, lines: &[IncompleteLine]) {
        self.incomplete_lines = lines.iter().rev().copied().collect();
    }

    /// Marks the records of each line that comes before `place`.
    pub(in crate::check) fn mark_incomplete_before(&mut self, place: u32) {
        while self.incomplete_lines.last().is_some_and(|line| line.span.lo < place) {
            if let Some(line) = self.incomplete_lines.pop() {
                self.mark_incomplete(line);
            }
        }
    }

    /// One line, with the messages of `pragincomplete`.
    fn mark_incomplete(&mut self, line: IncompleteLine) {
        let ty = match line.kind {
            Some(kind) => {
                let (kind, tag_kind) = match kind {
                    AstRecordKind::Struct => (rucc_types::RecordKind::Struct, TagKind::Struct),
                    AstRecordKind::Union => (rucc_types::RecordKind::Union, TagKind::Union),
                };
                match self.tag_use(Some(line.name), tag_kind, line.span) {
                    TagUse::Known(ty) => Some(ty),
                    found @ TagUse::New => {
                        let id = self.types.declare_record(kind, Some(line.name));
                        let ty = self.types.record(id);
                        self.bind_tag(found, Some(line.name), tag_kind, ty);
                        Some(ty)
                    }
                    TagUse::Wrong | TagUse::Anonymous => return,
                }
            }
            None => match self.scopes.lookup(line.name) {
                Some(Binding::Typedef(ty)) => Some(ty),
                Some(Binding::Decl(decl)) => Some(self.tast[decl].ty),
                Some(Binding::Enumerator { ty, .. }) => Some(ty),
                None => None,
            },
        };
        let spelled = self.text(line.name).to_owned();
        let Some(ty) = ty else {
            let what = format!("unknown type {spelled} in pragma incomplete");
            self.report(Diagnostic::error(what, line.span).with_code("E0798"));
            return;
        };
        match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Record(id) => {
                self.incomplete_records.insert(id);
            }
            _ => {
                let what = format!("not struct/union type in pragma incomplete: {spelled}");
                self.report(Diagnostic::error(what, line.span).with_code("E0798"));
            }
        }
    }

    /// The kencc signature of a type with the marks set so far, which `signof` gives.
    pub(in crate::check) fn kencc_signature(&self, ty: rucc_types::TypeId) -> u32 {
        let marked = &self.incomplete_records;
        kencc_signature(&self.types, ty, self.cx.target, &|id| marked.contains(&id))
    }

    /// The signature of each name with external linkage, as `sign` in `6c/swt.c` gives it: the
    /// signature of the type, or `SIGNINTERN` where that is zero. When a name is declared again,
    /// the last declaration gives the type, as `s->type` does in kencc.
    pub(in crate::check) fn kencc_signatures(&self) -> Map<String, u32> {
        let mut out = Map::default();
        for index in 0..self.tast.counts().decls {
            let decl = &self.tast[crate::decl::DeclId::from_usize(index)];
            let Some(name) = decl.name else { continue };
            if decl.linkage != Linkage::External {
                continue;
            }
            let value = match self.kencc_signature(decl.ty) {
                0 => KENCC_SIGN_INTERN,
                value => value,
            };
            out.insert(self.text(name).to_owned(), value);
        }
        out
    }
}
