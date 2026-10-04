//! `__attribute__((tls_model("...")))`, which asks for a thread-local variable to be reached
//! through one of the four sequences the ELF thread-local ABI has.
//!
//! glibc writes `initial-exec` on nearly every thread-local variable it has, through
//! `attribute_tls_model_ie`, and a library meant to be loaded early writes it to keep the call to
//! `__tls_get_addr` out of every access. The initial exec sequence is the one this compiler writes
//! for every thread-local variable, which `thread_address` in the code generator explains, so what
//! glibc asks for is what it gets. `local-exec` is a shorter sequence for the same thing, and the
//! initial exec one is right wherever that one is. The two dynamic models are the sequence that
//! calls `__tls_get_addr`, which this compiler does not write yet for any variable (issue #1104),
//! so a variable written with one is reached the way every other one is.
//!
//! So nothing about the attribute changes the code, and what is here is gcc 13's checking of it,
//! in its words and in its order: the argument count first, then whether it is written on a
//! thread-local variable at all, which is a warning and drops it, and then the string.

use rucc_ast::{AttrArg, AttrList};
use rucc_base::Symbol;
use rucc_diag::Diagnostic;

use rucc_lex::Encoding;

use crate::check::Checker;
use crate::expr::ExprKind;

/// What a `tls_model` attribute was written on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::check) enum Holder {
    /// A function or a member, which gcc says is not a variable.
    NotVariable,
    /// A variable without thread storage duration.
    NotThread,
    /// A thread-local variable, the one thing the attribute is for.
    Thread,
}

/// The four models gcc knows, in the order its message lists them.
const MODELS: [&str; 4] = ["local-exec", "initial-exec", "local-dynamic", "global-dynamic"];

impl Checker<'_> {
    /// Checks each `tls_model` in `lists`, written on `name`, the way gcc 13 does.
    pub(in crate::check) fn check_tls_model(
        &mut self,
        lists: &[AttrList],
        name: Option<Symbol>,
        holder: Holder,
    ) {
        for &list in lists {
            for attr in self.ast[list].to_vec() {
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
                    Holder::Thread => None,
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
                                Some(units.iter().filter_map(|&unit| char::from_u32(unit)).collect())
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
                if !MODELS.contains(&model.as_str()) {
                    let what = "'tls_model' argument must be one of 'local-exec', 'initial-exec', \
                                'local-dynamic', or 'global-dynamic'";
                    self.report(Diagnostic::error(what, attr.span).with_code("E0814"));
                }
            }
        }
    }
}
