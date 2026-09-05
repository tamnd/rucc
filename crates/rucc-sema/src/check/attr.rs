//! What an attribute means to a layout.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4 and `spec/12-abi-and-runtime.md` section 12.6.
//!
//! Two attributes change the size and the offsets of a record, and getting either of them wrong
//! is a miscompile rather than a missed optimization: a program that lays a structure over a
//! wire format or a hardware register is written against the layout the attribute asks for and
//! reads the wrong bytes without it. Those two are read here.
//!
//! `packed` takes the padding out. On a record it applies to every member, and on a member it
//! applies to that member alone, which is a difference the layout engine already holds.
//!
//! `aligned(n)` raises an alignment and never lowers it, which is the rule for the attribute on
//! its own. Written beside `packed` it is the way a program says both at once, as in
//! `__attribute__((packed, aligned(4)))`, and there the record is packed and then aligned to
//! four, which is not the same as either of them alone.
//!
//! `scalar_storage_order` is the third of that family and is the one that is refused. It reverses
//! the byte order of every scalar in the record, so a compiler that reads past it lays the record
//! out in the host's order and hands back every field with its bytes the wrong way round. There
//! is no harmless reading of it and no partial one, which is why it is an error here rather than
//! a warning: a program that does not build is a compiler that told the truth.
//!
//! Everything else in an attribute list is left where it is. An attribute nothing implements is
//! not this module's to complain about, since the same list is written on declarations that
//! have no layout at all.

use rucc_ast::{AlignSpec, AttrArg, AttrList};
use rucc_diag::{Diagnostic, Span};
use rucc_types::layout;

use crate::check::Checker;

/// What the layout engine takes from an attribute list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::check) struct Packing {
    /// Whether `packed` was written.
    pub(in crate::check) packed: bool,
    /// What `aligned` asked for, in bytes, and the largest of them where it was written twice.
    pub(in crate::check) align: Option<u32>,
}

/// What `aligned` with no argument asks for, which GCC calls `BIGGEST_ALIGNMENT`.
///
/// Sixteen on every target this compiler has, which is the alignment `long double` has on
/// x86-64 and the one the vector types have on aarch64 and riscv64. It is written here rather
/// than taken from [`rucc_target::TargetInfo`] because the target table has no field for it and
/// inventing one to hold a number that is the same everywhere would be describing a difference
/// that does not exist.
const BIGGEST_ALIGNMENT: u32 = 16;

/// The attributes that keep a definition nothing in the file refers to.
///
/// Every one of them says that something outside what the compiler can see reaches the
/// definition. `used` and `retain` say so in as many words, and are what a symbol a linker script
/// names is written with. `constructor` and `destructor` are called by the run-up to `main` and
/// the run-down after it, which is code no translation unit writes. `alias` gives a second name
/// to a definition, and the name is in a string that nothing resolves as a use.
const RETAINING: [&str; 5] = ["used", "retain", "constructor", "destructor", "alias"];

impl Checker<'_> {
    /// The `packed` and the `aligned` in an attribute list.
    ///
    /// Both spellings are read, since `[[gnu::packed]]` and `__attribute__((packed))` are the
    /// same attribute written two ways, and `__packed__` is read as `packed` because a header
    /// writes the armoured name so that a program's own macro cannot take the plain one. An
    /// attribute in some other namespace is not GCC's and is not read.
    pub(in crate::check) fn packing(&mut self, attrs: AttrList) -> Packing {
        let mut packing = Packing::default();
        // Copied out because folding the argument of `aligned` checks an expression, which
        // borrows the checker that the tree is being read through.
        let written = self.ast[attrs].to_vec();
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            match rucc_gnu::unarmour(self.text(attr.name)) {
                "packed" => packing.packed = true,
                "aligned" => {
                    if let Some(align) = self.aligned_argument(attr) {
                        packing.align = Some(packing.align.unwrap_or(1).max(align));
                    }
                }
                "scalar_storage_order" => {
                    let what = "'scalar_storage_order' is not implemented yet";
                    let note = "every scalar in this record would be read in the wrong byte order";
                    let refused = Diagnostic::error(what, attr.span).with_code("E0688");
                    self.report(refused.note(note, attr.span));
                }
                _ => {}
            }
        }
        packing
    }

    /// Whether an attribute list asks for the declaration to be kept where nothing refers to it.
    ///
    /// The armour and the namespace are read the same way [`Self::packing`] reads them. None of these five is implemented as anything else yet, and this is not that
    /// work: what it settles is only whether the definition exists, which is the one part of each
    /// of them that a program notices when the definition is dropped instead.
    pub(in crate::check) fn retains(&mut self, attrs: AttrList) -> bool {
        let written = self.ast[attrs].to_vec();
        for attr in written {
            if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu") {
                continue;
            }
            if RETAINING.contains(&rucc_gnu::unarmour(self.text(attr.name))) {
                return true;
            }
        }
        false
    }

    /// What an `alignas` on a member asked for, which is the same number `aligned` gives.
    ///
    /// C23 6.7.5 allows one on a member and the two spellings mean the same thing there, so this
    /// is the `_Alignas` half of the same wiring. Whether the number raises or lowers is the
    /// layout engine's to decide, and it raises.
    pub(in crate::check) fn member_alignas(
        &mut self,
        align: Option<AlignSpec>,
        span: Span,
    ) -> Option<u32> {
        let requested = match align? {
            AlignSpec::Type(named) => {
                let named = self.type_name(named);
                i128::from(layout(&self.types, named, self.cx.target).ok()?.align)
            }
            AlignSpec::Expr(expr) => {
                let value = self.expr(expr);
                match self.eval_integer(value) {
                    Ok(value) => value,
                    Err(failed) => {
                        if !failed.poisoned {
                            let at = self.tast.expr_span(failed.at);
                            let what = "requested alignment is not an integer constant";
                            self.report(Diagnostic::error(what, at).with_code("E0606"));
                        }
                        return None;
                    }
                }
            }
        };
        // C23 6.7.5p4 says `alignas(0)` has no effect, which is the one value below one that is
        // not a mistake, and it is the reason this is not the same test as the one above.
        if requested == 0 {
            return None;
        }
        if requested < 0 || requested & (requested - 1) != 0 {
            let what = format!("requested alignment '{requested}' is not a positive power of 2");
            self.report(Diagnostic::error(what, span).with_code("E0607"));
            return None;
        }
        u32::try_from(requested).ok()
    }

    /// What one `aligned` asked for, which is a number or nothing when it was written bare.
    fn aligned_argument(&mut self, attr: rucc_ast::Attribute) -> Option<u32> {
        let args = self.ast[attr.args].to_vec();
        let requested = match args.first() {
            None => return Some(BIGGEST_ALIGNMENT),
            Some(AttrArg::Expr(expr)) => {
                let value = self.expr(*expr);
                match self.eval_integer(value) {
                    Ok(value) => value,
                    Err(failed) => {
                        if !failed.poisoned {
                            let at = self.tast.expr_span(failed.at);
                            let what = "requested alignment is not an integer constant";
                            self.report(Diagnostic::error(what, at).with_code("E0606"));
                        }
                        return None;
                    }
                }
            }
            // `aligned(foo)` where `foo` is not an expression, which nothing writes and which
            // the parser keeps as an identifier because `format(printf, 1, 2)` does.
            Some(AttrArg::Ident(_)) => {
                let what = "requested alignment is not an integer constant";
                self.report(Diagnostic::error(what, attr.span).with_code("E0606"));
                return None;
            }
        };
        if requested <= 0 || requested & (requested - 1) != 0 {
            let what = format!("requested alignment '{requested}' is not a positive power of 2");
            self.report(Diagnostic::error(what, attr.span).with_code("E0607"));
            return None;
        }
        u32::try_from(requested).ok()
    }
}
