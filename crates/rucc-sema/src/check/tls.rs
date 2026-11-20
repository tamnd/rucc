//! `__attribute__((tls_model("...")))`, which asks for a thread-local variable to be reached
//! through one of the four sequences the ELF thread-local ABI has.
//!
//! glibc writes `initial-exec` on nearly every thread-local variable it has, through
//! `attribute_tls_model_ie`, and a library meant to be loaded early writes it to keep the call to
//! `__tls_get_addr` out of every access. The allocators need it most, because `__tls_get_addr` may
//! call `malloc` the first time a thread touches a block that `dlopen` added.
//!
//! The model is recorded on the variable, and the lowering puts it on the IR global. The code
//! generator then uses it or the fastest model the link allows, whichever is faster, as gcc does.
//! A variable with no attribute takes `-ftls-model=` instead.
//!
//! What is here is gcc 13's checking of the attribute, in its words and in its order: the argument
//! count first, then whether it is written on a thread-local variable at all, which is a warning
//! and drops it, and then the string.

use rucc_ast::{AttrArg, AttrList};
use rucc_base::Symbol;
use rucc_diag::Diagnostic;

use rucc_lex::Encoding;
use rucc_target::TlsModel;

use crate::check::Checker;
use crate::decl::DeclId;
use crate::expr::ExprKind;

/// What a `tls_model` attribute was written on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::check) enum Holder {
    /// A function or a member, which gcc says is not a variable.
    NotVariable,
    /// A variable without thread storage duration.
    NotThread,
    /// A thread-local variable, the one thing the attribute is for, and the model is recorded on
    /// it.
    Thread(DeclId),
}

impl Checker<'_> {
    /// Checks each `tls_model` in `lists`, written on `name`, the way gcc 13 does.
    pub(in crate::check) fn check_tls_model(
        &mut self,
        lists: &[AttrList],
        name: Option<Symbol>,
        holder: Holder,
    ) {
        let ast = self.ast;
        for &list in lists {
            for &attr in &ast[list] {
                if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu")
                    || rucc_gnu::unarmour(self.text(attr.name)) != "tls_model"
                {
                    continue;
                }
                let args = self.ast[attr.args].to_vec();
                let [arg] = args.as_slice() else {
                    let what = "wrong number of arguments specified for 'tls_model' attribute";
                    let note = format!("expected 1, found {}", args.len());
                    let refused = Diagnostic::error(what, attr.span).with_code("E0814");
                    self.report(refused.note(note, attr.span));
                    continue;
                };
                let spelled = name.map_or("", |name| self.text(name));
                let why = match holder {
                    Holder::NotVariable => Some(format!("'{spelled}' is not a variable")),
                    Holder::NotThread => {
                        Some(format!("'{spelled}' does not have thread storage duration"))
                    }
                    Holder::Thread(_) => None,
                };
                if let Some(why) = why {
                    let what = format!("'tls_model' attribute ignored because {why}");
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                    continue;
                }
                let model: Option<String> = match *arg {
                    AttrArg::Expr(expr) => {
                        let checked = self.expr(expr);
                        match self.tast[checked].kind {
                            ExprKind::Str(id) if self.tast[id].encoding == Encoding::Plain => {
                                let units = &self.tast[id].elements;
                                Some(
                                    units.iter().filter_map(|&unit| char::from_u32(unit)).collect(),
                                )
                            }
                            _ => None,
                        }
                    }
                    AttrArg::Ident(_) => None,
                };
                let Some(model) = model else {
                    let what = "'tls_model' argument not a string";
                    self.report(Diagnostic::error(what, attr.span).with_code("E0814"));
                    continue;
                };
                let Some(model) = TlsModel::from_gcc(&model) else {
                    let what = "'tls_model' argument must be one of 'local-exec', 'initial-exec', \
                                'local-dynamic', or 'global-dynamic'";
                    self.report(Diagnostic::error(what, attr.span).with_code("E0814"));
                    continue;
                };
                if let Holder::Thread(decl) = holder {
                    self.tast.record_tls_model(decl, model);
                }
            }
        }
    }
}
