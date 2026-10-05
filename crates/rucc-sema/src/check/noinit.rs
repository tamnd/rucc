//! `__attribute__((noinit))` and `__attribute__((persistent))`, gcc 12's two for a variable the
//! startup code leaves alone.
//!
//! A `noinit` variable is never cleared, so what a program wrote to it before a warm reset is
//! still there afterwards, and a `persistent` one is given its initial value by whatever loaded
//! the image and kept from then on. On ELF each has a section of its own name, `.noinit` with no
//! bytes in the file and `.persistent` with them, which a linker script for a board places outside
//! what the startup code clears and copies. That is the whole of what either does, and it is the
//! object writer's: here is gcc 13's checking of where the attribute may be written, in its words,
//! and the flag a variable that passes it carries.
//!
//! gcc reads the attributes of one declaration in the order they were written. One of the two
//! after a `section`, or after the other of the two, is ignored as conflicting with it before
//! anything else is asked of it, and a `section` after one of the two is ignored the same way.
//! What is left is asked whether it is on a variable at all, whether that variable is in a block
//! and automatic, which is an error, whether it is `const`, and whether it has the initializer
//! `persistent` needs and `noinit` must not have.

use rucc_ast::AttrList;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{Qualifiers, TypeId, TypeKind};

use crate::check::Checker;
use crate::decl::DeclFlags;

/// What gcc asks of the variable a `noinit` or `persistent` is written on.
#[derive(Clone, Copy)]
pub(in crate::check) struct Held {
    /// Whether it has an initializer in this declaration.
    pub initialized: bool,
    /// Whether it is automatic, which only a variable in a block can be.
    pub local: bool,
    /// Its type, which is asked whether it is `const`.
    pub ty: TypeId,
}

impl Checker<'_> {
    /// What gcc says about `noinit` and `persistent` in the lists of one declaration, in the
    /// order they were written, which is `None` for anything but a variable, and the flag for the
    /// one that is kept. The second answer is whether a `section` written after the kept one was
    /// ignored for it, which the caller drops.
    ///
    /// `at` is the declaration's name, which is where gcc says what it says about the variable.
    pub(in crate::check) fn reset_kept(
        &mut self,
        lists: &[AttrList],
        held: Option<Held>,
        at: Span,
    ) -> (DeclFlags, bool) {
        let ast = self.ast;
        let mut sectioned = false;
        let mut kept: Option<&'static str> = None;
        let mut unsectioned = false;
        for &attrs in lists {
            for &attr in &ast[attrs] {
                let name = self.gnu_name(&attr);
                if name == "section" {
                    match kept {
                        Some(kept) => {
                            let what = format!(
                                "ignoring attribute 'section' because it conflicts with \
                                 attribute '{kept}'"
                            );
                            self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                            unsectioned = true;
                        }
                        None => sectioned = true,
                    }
                    continue;
                }
                if name != "noinit" && name != "persistent" {
                    continue;
                }
                let count = ast[attr.args].len();
                if count > 0 {
                    let what = format!("wrong number of arguments specified for '{name}' attribute");
                    let refused = Diagnostic::error(what, attr.span).with_code("E0836");
                    self.report(refused.note(format!("expected 0, found {count}"), attr.span));
                    continue;
                }
                let before = if sectioned { Some("section") } else { kept };
                if let Some(before) = before.filter(|&before| before != name) {
                    let what = format!(
                        "ignoring attribute '{name}' because it conflicts with attribute '{before}'"
                    );
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                    continue;
                }
                let noinit = name == "noinit";
                let ignored = match held {
                    None => "not set on a variable",
                    Some(held) if held.local => {
                        let what = format!("'{name}' attribute cannot be specified for local variables");
                        self.report(Diagnostic::error(what, at).with_code("E0837"));
                        continue;
                    }
                    Some(held) if self.constant(held.ty) => "set on const variable",
                    Some(held) if noinit && held.initialized => "set on initialized variable",
                    Some(held) if !noinit && !held.initialized => "set on uninitialized variable",
                    Some(_) => {
                        kept = Some(name);
                        continue;
                    }
                };
                let what = format!("ignoring '{name}' attribute {ignored}");
                self.report(Diagnostic::warning(what, at).with_code("E0703"));
            }
        }
        let flags = match kept {
            Some("noinit") => DeclFlags::NOINIT,
            Some(_) => DeclFlags::PERSISTENT,
            None => DeclFlags::NONE,
        };
        (flags, unsectioned)
    }

    /// Whether a variable of this type is `const`, which for an array is its element's.
    fn constant(&self, ty: TypeId) -> bool {
        let mut ty = self.types.canonical(ty);
        loop {
            if self.types.quals(ty).has(Qualifiers::CONST) {
                return true;
            }
            match self.types.kind(ty) {
                TypeKind::Array { elem, .. } => ty = self.types.canonical(elem),
                _ => return false,
            }
        }
    }
}
