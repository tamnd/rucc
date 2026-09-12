//! `__builtin_prefetch`, which asks for an address to be brought closer before it is used.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! It is the one builtin here that promises nothing. A prefetch does not read the memory it names,
//! so nothing after it sees anything it would not have seen, and a target that writes no
//! instruction for it has implemented it correctly. What it changes is how long the program takes,
//! and a program that gets the address wrong is slower rather than wrong.
//!
//! That is also why it is a node rather than a call. There is no function of the name for a call
//! to reach, and before this there was not a node either, so every program that wrote one failed
//! to link on a name it never wrote. The wider defect is tamnd/rucc#303 and this row is one of the
//! four in tamnd/rucc#313.
//!
//! # The two arguments behind the address
//!
//! Both are optional and both have to be constants, which is gcc's rule and not a convenience: the
//! instruction is chosen by what they say, so a number that is not known until the program runs
//! has nothing to choose with. A call that writes one anyway is refused, in gcc's own words.
//!
//! The second says whether the access being prepared for will write. The third says how much of
//! the data will still be wanted afterwards, from zero for none of it to three for all of it, and
//! three is what the one argument form means.
//!
//! A constant outside the range is a warning rather than an error and the argument is read as
//! zero, which is again what gcc 16.2.0 does. Measured rather than reasoned about: `-O2 -Wall`
//! over the eight shapes gives `invalid second argument to '__builtin_prefetch'; using zero` for a
//! second argument of five, the same sentence about the third for a third of nine, and an error
//! for either of them not being a constant at all.
//!
//! # What happens to a fourth argument and beyond
//!
//! The row's signature ends in `...`, so a call that writes more is not refused by the prototype,
//! and gcc does not refuse it either. What it means is nothing, since there is no fourth thing to
//! say about a prefetch. They are kept here rather than dropped, in a comma in front of the node,
//! so that a side effect somebody wrote inside one still happens. gcc appears to drop them, and
//! the shape is rare enough that keeping what the program wrote is the safer of the two.

use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// The name, which is the whole family.
const NAME: &str = "__builtin_prefetch";

/// The code the arguments of this builtin report under.
const CODE: &str = "E0704";

/// The locality a call that did not say means, which is all of the data wanted afterwards.
const MOST: i128 = 3;

impl Checker<'_> {
    /// The node for a call to `__builtin_prefetch`, if the name is one.
    ///
    /// Answers nothing for every other call in the program, which is every call, so the test that
    /// costs a byte goes first. The name decides this and not the declaration, the way it does for
    /// its neighbours: what `__builtin_prefetch` means is a fact about the name, since the prefix
    /// is what says the name belongs to the implementation.
    pub(in crate::check) fn prefetch_builtin(
        &mut self,
        function: Option<Symbol>,
        args: &[ExprId],
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_") || spelled != NAME {
            return None;
        }
        // No address at all, which the prototype has already refused. The ordinary node is built
        // and the program is refused for the reason it was already going to be refused for.
        let &address = args.first()?;
        if args.iter().any(|&arg| self.is_poisoned(arg)) {
            return Some(self.poison(span));
        }
        let write = self.prefetch_argument(args.get(1).copied(), "second", 1) == 1;
        let locality = self.prefetch_argument(args.get(2).copied(), "third", MOST);
        let node = ExprKind::Prefetch {
            address,
            write,
            // In range by construction: `prefetch_argument` answers zero for everything outside it.
            locality: u8::try_from(locality).unwrap_or(0),
        };
        let ty = self.types.void();
        let mut answer = self.tast.expr(Expr::new(node, ty, Category::Rvalue), span);

        // Whatever the node did not take, backwards so that they run in the order they were
        // written: a comma runs its left side first, so wrapping from the inside out puts the
        // first of them outermost.
        for &rest in args.iter().skip(3).rev() {
            let node = ExprKind::Comma { lhs: rest, rhs: answer };
            answer = self.tast.expr(Expr::new(node, ty, Category::Rvalue), span);
        }
        Some(answer)
    }

    /// One of the two constants behind the address, as a number in range.
    ///
    /// `most` is the largest the argument may be, and the smallest is always zero. A call that did
    /// not write the argument at all means `most`, since both defaults are the top of their range:
    /// a prefetch that says nothing is a read that wants all of the data afterwards.
    ///
    /// Zero for an argument that would not fold or that folded outside the range, which is what
    /// gcc reads it as, and a diagnostic either way.
    fn prefetch_argument(&mut self, arg: Option<ExprId>, which: &str, most: i128) -> i128 {
        let Some(arg) = arg else { return most };
        let span = self.tast.expr_span(arg);
        // The folding is asked and its own diagnostics are dropped, because what is reported here
        // is that the argument is not a constant, in the words of the builtin it is an argument of.
        let Ok(value) = self.eval().integer(arg) else {
            let what = format!("the {which} argument of `{NAME}` is a constant or it is nothing");
            let note = "the instruction this becomes is chosen by what it says, so a number that \
                        is not known until the program runs has nothing to choose with";
            self.report(Diagnostic::error(what, span).with_code(CODE).note(note, span));
            return 0;
        };
        if value < 0 || value > most {
            let what = format!("the {which} argument of `{NAME}` is nothing above {most}");
            let note = format!("`{value}` is read as zero here, which is what gcc reads it as");
            self.report(Diagnostic::warning(what, span).with_code(CODE).note(note, span));
            return 0;
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// The name has to be a row of the table with a signature, because the signature is what the
    /// call is checked against before this replaces it. A row without one would never be declared
    /// and the call would be to an undeclared name.
    #[test]
    fn the_name_is_a_row_of_the_table_that_carries_a_signature() {
        let Some(feature) = rucc_gnu::lookup(Kind::Builtin, NAME) else {
            panic!("{NAME} is answered here and is not in features.toml");
        };
        assert_eq!(feature.status, Status::Implemented);
        assert!(!feature.signature.is_empty(), "{NAME} is checked against its prototype");
        assert!(feature.library.is_empty(), "{NAME} is not a call to anything");
        assert!(
            feature.signature.contains("..."),
            "the two arguments behind the address are optional, so the prototype has to let a \
             call write one, two or three of them: {}",
            feature.signature
        );
    }
}
