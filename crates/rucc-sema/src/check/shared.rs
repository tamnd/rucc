//! `__attribute__((shared))`, gcc's word on x86 Windows that the section a variable is in is one
//! copy that every process running the image shares rather than a copy each.
//!
//! What it changes is a flag on the section, the `s` in `.section name,"dws"` and
//! `IMAGE_SCN_MEM_SHARED` in the object, which the linker carries into the image for the loader
//! to read. Every other variable in that section is shared along with it, so it is meant to go
//! with a `section` of its own, as gcc's manual says. Here is gcc's checking of where it may be
//! written, in its words, and the flag a variable that passes it carries.

use rucc_ast::AttrList;
use rucc_diag::Diagnostic;

use crate::check::Checker;
use crate::decl::{DeclFlags, DeclKind};

impl Checker<'_> {
    /// What gcc says about `shared` in the lists of one declaration of this kind, and the flag
    /// when it is kept, which is on a variable alone.
    ///
    /// gcc has the attribute in its PE back end for x86, so on any other target it is a name gcc
    /// has never heard of and says so.
    pub(in crate::check) fn shared_section(
        &mut self,
        lists: &[AttrList],
        kind: DeclKind,
    ) -> DeclFlags {
        let tuple = self.cx.target.tuple;
        let here = matches!(tuple.arch().as_str(), "x86_64" | "i686")
            && tuple.os().as_str() == "windows";
        let ast = self.ast;
        let mut flags = DeclFlags::NONE;
        for &attrs in lists {
            for &attr in &ast[attrs] {
                if self.gnu_name(&attr) != "shared" {
                    continue;
                }
                if !here {
                    let what = match attr.namespace {
                        Some(_) => "'gnu::shared' scoped attribute directive ignored",
                        None => "'shared' attribute directive ignored",
                    };
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                    continue;
                }
                let count = ast[attr.args].len();
                if count > 0 {
                    let what = "wrong number of arguments specified for 'shared' attribute";
                    let refused = Diagnostic::error(what, attr.span).with_code("E0844");
                    self.report(refused.note(format!("expected 0, found {count}"), attr.span));
                } else if kind == DeclKind::Object {
                    flags |= DeclFlags::SHARED;
                } else {
                    let what = "'shared' attribute only applies to variables";
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                }
            }
        }
        flags
    }
}
