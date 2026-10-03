//! `#pragma redefine_extname`, applied by name once the whole file has been checked.
//!
//! The parser reads the lines and this applies them, as `__asm__("new")` on the one declaration
//! every `extern` of the name shares, so a call written above the line goes to the new name as
//! well as one written below it, which is what gcc does for a declaration. A name with internal
//! linkage, or one the file never declares, is left alone without a word, as in gcc, and a name
//! that already has another assembler name keeps it, with gcc's `-Wpragmas` warning.
//!
//! gcc also leaves alone a function whose first declaration is its definition below the line, and
//! warns about one defined above it, since its assembler name is settled when the body is. Those
//! two are left out here, for clang's reading: the checker has no order between a line and a
//! declaration in another file to compare, and the name is the same name in either place.

use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};

use crate::check::Checker;
use crate::decl::Linkage;

impl Checker<'_> {
    /// Applies one `#pragma redefine_extname` line, `span` being where it wrote the word.
    pub fn pragma_extname(&mut self, old: Symbol, new: Symbol, span: Span) {
        let Some(decl) = self.file_scope_decl(old) else { return };
        let mut node = self.tast[decl].clone();
        if node.linkage != Linkage::External {
            return;
        }
        if let Some(label) = node.asm_label {
            let spelled: String =
                self.tast[label].elements.iter().filter_map(|&unit| char::from_u32(unit)).collect();
            if spelled != self.text(new) {
                let what =
                    "`#pragma redefine_extname` ignored due to conflict with previous rename";
                self.report(Diagnostic::warning(what, span).with_code("E0798"));
            }
            return;
        }
        node.asm_label = Some(self.symbol_string(new));
        self.tast.set_decl(decl, node);
    }
}
