//! `__builtin_frame_address` and `__builtin_return_address`, which ask about the frames the
//! program is running in.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! Both take one argument, which is how many frames up from this one to look, and both answer a
//! pointer. A depth of zero is this function's own frame. Everything above that is a walk up the
//! chain of saved frame pointers, one link at a time, and the two builtins differ only in what is
//! read at the end of the walk: the frame itself, or the address control goes back to from it.
//!
//! That is why they are one node with a question rather than two nodes, and it is why they are a
//! node at all rather than a call. There is no function of either name anywhere for a call to
//! reach, so a program that wrote one used to fail to link on a name it never wrote itself.
//!
//! # Why the depth has to be a constant
//!
//! The same reason `check/builtin/prefetch.rs` gives for its two: what the call becomes is a walk
//! that many links long, written out, so a number that is not known until the program runs has
//! nothing to walk. gcc 16.2.0 refuses one too, with `invalid argument to
//! '__builtin_return_address'`, measured rather than reasoned about.
//!
//! # Why a depth above the limit is refused
//!
//! gcc writes the whole walk however long it is: measured at 16.2.0, `__builtin_frame_address` of
//! a thousand is a thousand loads. There is no program that wants this. A chain of frames that
//! deep either does not exist, in which case the walk reads whatever the stack happens to hold and
//! faults somewhere in the middle of it, or it does and the answer is about a caller a thousand
//! removed. Refusing it is better than a compilation that takes a gigabyte of instructions to say
//! something the program cannot use, which is the one place this and gcc part company, and the
//! limit is far above any depth a real program writes.
//!
//! The signature is `void *(unsigned int)`, so a negative depth is a large one by the time it gets
//! here rather than a negative one, and it is refused by the limit along with every other number
//! that size.

use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::TypeId;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind, FrameAsk};

/// The two names, with what each of them asks for.
const NAMES: [(&str, FrameAsk); 2] =
    [("__builtin_frame_address", FrameAsk::Frame), ("__builtin_return_address", FrameAsk::Return)];

/// The code the argument of either builtin reports under.
const CODE: &str = "E0705";

/// The deepest walk that is written, which is far above what any program asks for and low enough
/// that the instructions it becomes stay a number a person could read.
const DEEPEST: i128 = 255;

impl Checker<'_> {
    /// The node for a call to one of the two, if the name is one of them.
    ///
    /// Answers nothing for every other call in the program, which is every call, so the test that
    /// costs a byte goes first. The name decides this and not the declaration, the way it does for
    /// its neighbours: the reserved prefix is what says the name belongs to the implementation.
    ///
    /// The type is taken from the call rather than built here, so that the one place saying what
    /// this answers with is the row in the table the call was checked against.
    pub(in crate::check) fn frame_address_builtin(
        &mut self,
        function: Option<Symbol>,
        args: &[ExprId],
        ret: TypeId,
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_") {
            return None;
        }
        let (name, ask) = NAMES.iter().copied().find(|&(named, _)| named == spelled)?;
        // No depth at all, which the prototype has already refused. The ordinary node is built and
        // the program is refused for the reason it was already going to be refused for.
        let &depth = args.first()?;
        if self.is_poisoned(depth) {
            return Some(self.poison(span));
        }
        let Some(depth) = self.frame_depth(name, depth) else {
            return Some(self.poison(span));
        };
        let node = ExprKind::FrameAddress { ask, depth };
        Some(self.tast.expr(Expr::new(node, ret, Category::Rvalue), span))
    }

    /// How far up the argument says to walk, as a number the walk can be written from.
    ///
    /// Nothing for an argument that would not fold or that folded outside the range, which is a
    /// call that has been reported and whose value the caller has nothing to build.
    fn frame_depth(&mut self, name: &str, arg: ExprId) -> Option<u32> {
        let span = self.tast.expr_span(arg);
        // The folding is asked and its own diagnostics are dropped, because what is reported here
        // is that the argument is not a constant, in the words of the builtin it is an argument of.
        let Ok(value) = self.eval().integer(arg) else {
            let what = format!("the argument of `{name}` is a constant or it is nothing");
            let note = "what this becomes is a walk up that many frames, written out, so a number \
                        that is not known until the program runs has nothing to walk";
            self.report(Diagnostic::error(what, span).with_code(CODE).note(note, span));
            return None;
        };
        if !(0..=DEEPEST).contains(&value) {
            let what = format!("the argument of `{name}` is nothing above {DEEPEST}");
            let note = format!(
                "`{value}` frames up is not somewhere a program can ask about: the walk reads \
                 whatever the stack holds and faults part way up it"
            );
            self.report(Diagnostic::error(what, span).with_code(CODE).note(note, span));
            return None;
        }
        // In range by the test above, whose top is far below what a `u32` holds.
        u32::try_from(value).ok()
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Both names have to be rows of the table with a signature, because the signature is what the
    /// call is checked against before this replaces it, and both have to be implemented, because
    /// that is what stops the lowering refusing the call before it ever gets here.
    #[test]
    fn both_names_are_rows_of_the_table_that_carry_a_signature() {
        for (name, _) in NAMES {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented, "{name}");
            assert_eq!(feature.signature, "void *(unsigned int)", "{name}");
            assert!(feature.library.is_empty(), "{name} is not a call to anything");
        }
    }
}
