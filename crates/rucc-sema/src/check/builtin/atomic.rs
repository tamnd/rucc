//! The atomic accesses and the barrier: `__atomic_load_n` and its neighbours.
//!
//! Design: `spec/13-gnu-compat.md` section 13.6, and tamnd/rucc#311.
//!
//! Four names here, out of a family of forty two. `__atomic_load_n` reads an object, and
//! `__atomic_store_n` writes one, both without tearing and both with an ordering that says what
//! may be moved across them. `__atomic_thread_fence` is that ordering with no access attached, and
//! `__sync_synchronize` is the same barrier at sequential consistency under the older family's
//! spelling.
//!
//! SQLite is why these four and not some other four. Its `AtomicLoad` and `AtomicStore` macros are
//! `__atomic_load_n` and `__atomic_store_n` at relaxed ordering, it calls `__sync_synchronize`
//! directly twice, and those three names are the whole of what an amalgamation build asks for.
//!
//! # Why they are nodes
//!
//! An ordering is not an argument. It is something the IR says about an access, the way an
//! alignment is, and there is no function anywhere that a call could reach: no object file defines
//! `__atomic_load_n`, and if one did, a call to it would be a call and a call is exactly the thing
//! an ordering has to be able to constrain. So the call becomes a node, the same way the byte swaps
//! and the overflow checks do, and the walk to the IR builds an access with the ordering on it.
//!
//! # The ordering has to be a constant
//!
//! C says the argument is an `int` and does not say it is constant, so a program may write one the
//! compiler cannot fold. gcc treats that as sequential consistency, which is the only safe reading:
//! the ordering has to be decided before the program runs, and the strongest one is right whatever
//! the program would have passed. This does the same.
//!
//! A number that names no ordering, or an ordering the operation cannot have, is W0333 and is then
//! taken as sequential consistency for the same reason. gcc warns rather than refusing here, and a
//! refusal would break the macro-heavy code this family appears in, where an argument is often a
//! macro that expands differently per platform.
//!
//! # What is not here
//!
//! `__atomic_load` and `__atomic_store`, the forms that write through a second pointer rather than
//! answering, which nothing measured uses. The read-modify-writes and the compare and exchange,
//! which need a `lock` prefix in the instruction description and are the rest of tamnd/rucc#311.
//! And `__atomic_signal_fence`, which orders against a signal handler on the same thread and so has
//! to constrain the compiler while emitting no instruction at all. The IR's `fence` is a machine
//! barrier, so spelling a signal fence as one would be correct and would cost an `mfence` that
//! nothing needs. It waits for a barrier that says what it means.

use rucc_diag::{Diagnostic, Span};
use rucc_types::pointee;

use crate::check::Checker;
use crate::expr::{AtomicOp, Category, Expr, ExprId, ExprKind, Ordering};
use crate::tast::Const;

/// The names that reach this through the type generic table, and what each one is.
///
/// `__sync_synchronize` is not here because it carries a signature, so it is checked as an ordinary
/// call and is answered by [`Checker::sync_builtin_value`] instead.
const FAMILY: &[(&str, AtomicOp)] = &[
    ("__atomic_load_n", AtomicOp::Load),
    ("__atomic_store_n", AtomicOp::Store),
    ("__atomic_thread_fence", AtomicOp::Fence),
];

/// The one name of the older family that is not type generic.
const SYNCHRONIZE: &str = "__sync_synchronize";

/// The numbers `<stdatomic.h>` and gcc's own headers give the orderings, in the order gcc gives
/// them.
///
/// `memory_order_consume` is the second, and it becomes [`Ordering::Acquire`] here. Every compiler
/// in use gives the two the same code, and a spelling of consume that means acquire would be a name
/// whose only effect is to make a reader think it was implemented.
const NUMBERED: &[Ordering] = &[
    Ordering::Relaxed,
    Ordering::Acquire,
    Ordering::Acquire,
    Ordering::Release,
    Ordering::AcqRel,
    Ordering::SeqCst,
];

/// Which shape a type generic name is, if it is one of these.
pub(in crate::check) fn shape(spelled: &str) -> Option<AtomicOp> {
    FAMILY.iter().find(|&&(name, _)| name == spelled).map(|&(_, op)| op)
}

/// Whether an operation of this shape can carry this ordering.
///
/// A load cannot release, because it wrote nothing for anybody to see, and a store cannot acquire,
/// because it read nothing to synchronise with. A barrier can be any of them, including relaxed,
/// which orders nothing and is what a program writes when the ordering is a macro that came out
/// relaxed on this platform.
fn allowed(op: AtomicOp, order: Ordering) -> bool {
    match op {
        AtomicOp::Load => matches!(order, Ordering::Relaxed | Ordering::Acquire | Ordering::SeqCst),
        AtomicOp::Store => {
            matches!(order, Ordering::Relaxed | Ordering::Release | Ordering::SeqCst)
        }
        AtomicOp::Fence => true,
    }
}

impl Checker<'_> {
    /// The node one of the type generic atomics becomes, once its arguments have been checked.
    ///
    /// The arguments arrive as values in the types they were written with. The one that has to be
    /// converted is the value being stored, which becomes the type of the object it is going into,
    /// because that is the width of the access and a store of a `char` through an `int *` is a four
    /// byte write.
    pub(in crate::check) fn atomic_builtin(
        &mut self,
        op: AtomicOp,
        spelled: &str,
        args: &[ExprId],
        span: Span,
    ) -> ExprId {
        // The ordering is the last argument of all three, which is the shape the whole family has:
        // the object comes first, whatever it is being handed comes next, and how strongly it is
        // ordered comes last.
        let Some(&written) = args.last() else { return self.poison(span) };
        let order = self.ordering(op, written, spelled);

        let operands = match op {
            AtomicOp::Fence => Vec::new(),
            AtomicOp::Load => vec![args[0]],
            AtomicOp::Store => {
                let target = self.accessed(args[0]);
                vec![args[0], self.conv().to_type(args[1], target)]
            }
        };
        let ty = match op {
            AtomicOp::Load => self.accessed(args[0]),
            AtomicOp::Store | AtomicOp::Fence => self.types.void(),
        };
        let args = self.tast.add_expr_refs(&operands);
        self.tast.expr(Expr::new(ExprKind::Atomic { op, order, args }, ty, Category::Rvalue), span)
    }

    /// `__sync_synchronize()`, which is a full barrier and takes nothing.
    ///
    /// Answers nothing for every other call in the program, so the test that costs a byte goes
    /// first. It is answered here rather than beside the three above because it has a signature in
    /// the table and so is checked against a prototype like any other call, which is the older
    /// family's one member that could be.
    pub(in crate::check) fn sync_builtin_value(
        &mut self,
        function: Option<rucc_base::Symbol>,
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__sync_") || spelled != SYNCHRONIZE {
            return None;
        }
        let ty = self.types.void();
        let args = self.tast.add_expr_refs(&[]);
        let kind = ExprKind::Atomic { op: AtomicOp::Fence, order: Ordering::SeqCst, args };
        Some(self.tast.expr(Expr::new(kind, ty, Category::Rvalue), span))
    }

    /// The type an access through this pointer touches, with the qualifiers off it.
    ///
    /// The argument has already been checked to be a pointer to something that is not `void`, by
    /// `object_type` in `check/builtin/generic.rs`, so the fallback here is unreachable in a program
    /// that got this far and is written rather than asserted because a poisoned argument can reach
    /// it and has already been complained about.
    fn accessed(&mut self, object: ExprId) -> rucc_types::TypeId {
        match pointee(&self.types, self.tast[object].ty) {
            Some(target) => self.plain(target),
            None => self.tast[object].ty,
        }
    }

    /// The ordering the source asked for, checked against what the operation can carry.
    ///
    /// Everything that is not an ordering this operation can have comes back as sequential
    /// consistency, which is stronger than anything the program could have meant and so is the one
    /// answer that cannot make a working program wrong.
    fn ordering(&mut self, op: AtomicOp, written: ExprId, spelled: &str) -> Ordering {
        if self.is_poisoned(written) {
            return Ordering::SeqCst;
        }
        let mut eval = self.eval();
        let folded = eval.constant(written);
        // The messages folding produced are dropped rather than reported. A non-constant argument
        // is allowed here, and what folding says about one is that it is not a constant, which is
        // not news to anybody and is not a complaint about this program.
        let _ = eval.finish();
        let at = self.tast.expr_span(written);
        let Ok(Const::Int(number)) = folded else {
            return Ordering::SeqCst;
        };
        let known = usize::try_from(number).ok().and_then(|index| NUMBERED.get(index).copied());
        let Some(order) = known.filter(|&order| allowed(op, order)) else {
            self.report(
                Diagnostic::warning(
                    format!(
                        "{number} is not a memory order '{spelled}' can be given, so this is \
                         ordered as if it were sequentially consistent"
                    ),
                    at,
                )
                .with_code("W0333"),
            );
            return Ordering::SeqCst;
        };
        order
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// The three type generic names have to be rows of the roster that carry no signature, or the
    /// ordinary call checking would answer for them before this ever sees them.
    #[test]
    fn the_generic_names_are_rows_of_the_table_that_carry_no_signature() {
        for &(name, _) in FAMILY {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented, "{name}");
            assert!(feature.signature.is_empty(), "{name} has a signature and is type generic");
        }
    }

    /// The older family's one member with a signature, which is the opposite requirement.
    #[test]
    fn the_barrier_of_the_older_family_is_a_row_that_carries_one() {
        let feature = rucc_gnu::lookup(Kind::Builtin, SYNCHRONIZE).expect("a row of features.toml");
        assert_eq!(feature.status, Status::Implemented);
        assert!(!feature.signature.is_empty(), "it is checked against its prototype");
        assert!(feature.library.is_empty(), "it is not a call to anything");
    }

    /// The numbers are gcc's and glibc's, and getting one of them wrong would turn a release into
    /// an acquire without anything noticing, so they are written out rather than counted.
    #[test]
    fn the_numbers_are_the_ones_the_headers_use() {
        assert_eq!(NUMBERED[0], Ordering::Relaxed);
        assert_eq!(NUMBERED[2], Ordering::Acquire);
        assert_eq!(NUMBERED[3], Ordering::Release);
        assert_eq!(NUMBERED[4], Ordering::AcqRel);
        assert_eq!(NUMBERED[5], Ordering::SeqCst);
        assert_eq!(NUMBERED.len(), 6);
    }

    /// Consume is the one that is not itself, and it is worth its own test because the reason is a
    /// decision rather than a fact about the numbering.
    #[test]
    fn consume_is_read_as_acquire() {
        assert_eq!(NUMBERED[1], Ordering::Acquire);
    }

    /// A load cannot release and a store cannot acquire, and both of those are things a program
    /// reaches by passing a macro that came out of some other platform's header.
    #[test]
    fn an_operation_refuses_the_orderings_it_has_nothing_to_say_about() {
        assert!(!allowed(AtomicOp::Load, Ordering::Release));
        assert!(!allowed(AtomicOp::Load, Ordering::AcqRel));
        assert!(!allowed(AtomicOp::Store, Ordering::Acquire));
        assert!(!allowed(AtomicOp::Store, Ordering::AcqRel));
    }

    /// And what each of them can carry, including relaxed, which orders nothing and is what SQLite
    /// writes.
    #[test]
    fn every_operation_takes_the_orderings_it_means_something_for() {
        assert!(allowed(AtomicOp::Load, Ordering::Relaxed));
        assert!(allowed(AtomicOp::Load, Ordering::Acquire));
        assert!(allowed(AtomicOp::Load, Ordering::SeqCst));
        assert!(allowed(AtomicOp::Store, Ordering::Relaxed));
        assert!(allowed(AtomicOp::Store, Ordering::Release));
        assert!(allowed(AtomicOp::Store, Ordering::SeqCst));
        for &order in NUMBERED {
            assert!(allowed(AtomicOp::Fence, order), "a barrier takes {order:?}");
        }
    }

    /// A name outside the family asks for nothing, including the ones spelled almost the same way
    /// that are the rest of tamnd/rucc#311.
    #[test]
    fn a_name_outside_the_family_asks_for_nothing() {
        assert_eq!(shape("__atomic_load_n"), Some(AtomicOp::Load));
        assert_eq!(shape("__atomic_store_n"), Some(AtomicOp::Store));
        assert_eq!(shape("__atomic_thread_fence"), Some(AtomicOp::Fence));
        assert_eq!(shape("__atomic_load"), None);
        assert_eq!(shape("__atomic_store"), None);
        assert_eq!(shape("__atomic_signal_fence"), None);
        assert_eq!(shape("__atomic_fetch_add"), None);
        assert_eq!(shape(SYNCHRONIZE), None);
    }
}
