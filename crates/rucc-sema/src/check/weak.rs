//! `#pragma weak`, applied by name once the whole file has been checked.
//!
//! The parser reads the lines and this applies them, after the last declaration rather than
//! where each line stands, because gcc applies one written above the declaration it names and
//! one written below it alike. `#pragma weak name` is `weak` on the name, which is refused on
//! one with internal linkage as the attribute is, and does nothing to a name the file never
//! declares, as in gcc. `#pragma weak name = target` is `name` declared weak with
//! `alias("target")`, and a `name` the file never declares is declared here, with the type of
//! the target, so that the object still gets the second symbol.

use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_lex::{Encoding, Remarks, StringLiteral};
use rucc_types::IntKind;

use crate::check::Checker;
use crate::decl::{
    Decl, DeclFlags, DeclId, DeclKind, DeclList, Definition, Effects, Emission, Linkage, Startup,
    StorageDuration,
};
use crate::scope::Binding;
use crate::tast::StrId;

impl Checker<'_> {
    /// Applies one `#pragma weak` line, `span` being where it wrote the name.
    pub fn pragma_weak(&mut self, name: Symbol, target: Option<Symbol>, span: Span) {
        let alias = target.map(|target| self.symbol_string(target));
        let Some(decl) = self.file_scope_decl(name) else {
            if let Some(alias) = alias {
                self.declare_weak_alias(name, target, alias, span);
            }
            return;
        };
        let mut node = self.tast[decl].clone();
        if node.linkage == Linkage::Internal {
            let spelled = self.text(name).to_owned();
            let what = format!("weak declaration of '{spelled}' must be public");
            let at = self.tast.decl_span(decl);
            let note = "'static' keeps the name inside this file, so no other object can define it";
            self.report(Diagnostic::error(what, at).with_code("E0711").note(note, span));
            return;
        }
        node.flags |= DeclFlags::WEAK;
        // A name the file defines is not a second name for anything, and an alias the
        // declarations already gave it is the first one written, which stands.
        if node.state == Definition::Declared && node.alias.is_none() {
            node.alias = alias;
        }
        self.tast.set_decl(decl, node);
    }

    /// The file-scope declaration of `name`, including one only a block-scope `extern` made.
    fn file_scope_decl(&self, name: Symbol) -> Option<DeclId> {
        match self.scopes.lookup(name) {
            Some(Binding::Decl(decl)) if self.tast[decl].linkage != Linkage::None => Some(decl),
            _ => self.out_of_sight.get(&name).copied(),
        }
    }

    /// Declares `name` as a weak second name for `target`, as the line asked for a name the file
    /// never declared. It takes the target's type, and `int` when the file declares no target
    /// either, which is then the undefined target lowering reports.
    fn declare_weak_alias(
        &mut self,
        name: Symbol,
        target: Option<Symbol>,
        alias: StrId,
        span: Span,
    ) {
        let like = target.and_then(|target| self.file_scope_decl(target));
        let (ty, kind, params) = match like {
            Some(like) => {
                let node = &self.tast[like];
                (node.ty, node.kind, node.params)
            }
            None => (self.types.int(IntKind::Int), DeclKind::Object, DeclList::EMPTY),
        };
        let node = Decl {
            name: Some(name),
            ty,
            kind,
            linkage: Linkage::External,
            duration: StorageDuration::Static,
            state: Definition::Declared,
            alignment: None,
            flags: DeclFlags::WEAK,
            asm_label: None,
            register: None,
            alias: Some(alias),
            inline: Emission::External,
            effects: Effects::Any,
            visibility: None,
            startup: Startup::default(),
            init: None,
            cleanup: None,
            params,
            body: None,
        };
        let id = self.tast.decl(node, span);
        self.scopes.declare(name, Binding::Decl(id));
        self.tast.add_top_level(id);
    }

    /// A name as the string an `alias` attribute would have written for it.
    fn symbol_string(&mut self, name: Symbol) -> StrId {
        let elements = self.text(name).chars().map(|c| c as u32).collect();
        self.tast.add_string(StringLiteral {
            elements,
            encoding: Encoding::Plain,
            remarks: Remarks::default(),
        })
    }
}
