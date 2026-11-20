//! `__attribute__((nodirect_extern_access))`, gcc 12's promise that a name is never reached from
//! the code as though it were in the same image.
//!
//! A variable another object defines is normally reached directly from an executable, and the
//! linker copies it into the executable to make that work, which is what a library that wants to
//! keep its variable to itself, protected, cannot have. Written on the declaration such a library's
//! header gives the variable, the attribute has its address read out of the global offset table
//! instead, even in position dependent code, and the address of a function so declared the same
//! way. What is read is the code generator's; here is gcc 13's checking of where it may be
//! written, in its words, and the flag a name that passes it carries.

use rucc_ast::AttrList;
use rucc_diag::{Diagnostic, Span};

use crate::check::Checker;
use crate::decl::DeclFlags;

impl Checker<'_> {
    /// What gcc says about `nodirect_extern_access` in the lists of one declaration, and the flag
    /// when it is kept.
    ///
    /// `public` is `None` for anything but a variable or a function, which is a member, a typedef
    /// or a parameter, and the attribute is ignored there. Otherwise it is whether the name has
    /// external linkage, and a name that does not, a `static` one or one in a block that is not
    /// `extern`, is not one the attribute does anything for. gcc says both of those and the wrong
    /// number of arguments at `at`, which is where the declaration begins.
    ///
    /// Read on x86 alone, where gcc has it.
    pub(in crate::check) fn extern_access(
        &mut self,
        lists: &[AttrList],
        public: Option<bool>,
        at: Span,
    ) -> DeclFlags {
        if !matches!(self.cx.target.tuple.arch().as_str(), "x86_64" | "i686") {
            return DeclFlags::NONE;
        }
        let ast = self.ast;
        let mut flags = DeclFlags::NONE;
        for &attrs in lists {
            for &attr in &ast[attrs] {
                if self.gnu_name(&attr) != "nodirect_extern_access" {
                    continue;
                }
                let count = ast[attr.args].len();
                if count > 0 {
                    let what = "wrong number of arguments specified for 'nodirect_extern_access' \
                                attribute";
                    let refused = Diagnostic::error(what, at).with_code("E0838");
                    self.report(refused.note(format!("expected 0, found {count}"), at));
                    continue;
                }
                match public {
                    None => {
                        let what = "'nodirect_extern_access' attribute ignored";
                        self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                    }
                    Some(false) => {
                        let what =
                            "'nodirect_extern_access' attribute have effect only on public objects";
                        self.report(Diagnostic::warning(what, at).with_code("E0703"));
                    }
                    Some(true) => flags |= DeclFlags::NODIRECT,
                }
            }
        }
        flags
    }
}
