//! The atomic accesses and the barrier: `__atomic_load_n` and its neighbours.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5, and tamnd/rucc#311.
//!
//! Thirty seven names here, out of a family of forty two. `__atomic_load_n` reads an object, and
//! `__atomic_store_n` writes one, both without tearing and both with an ordering that says what
//! may be moved across them. `__atomic_thread_fence` is that ordering with no access attached, and
//! `__sync_synchronize` is the same barrier at sequential consistency under the older family's
//! spelling. `__atomic_always_lock_free` and `__atomic_is_lock_free` are not operations at all:
//! they ask whether an object of a given size is one the machine handles without a lock, and both
//! are constants worked out here.
//!
//! Four compare and exchange. `__atomic_compare_exchange_n` and `__atomic_compare_exchange` are the
//! C11 family's, and `__sync_bool_compare_and_swap` and `__sync_val_compare_and_swap` are the older
//! one's. All four are the same instruction and differ in what they answer and in whether the value
//! expected arrived by pointer or by value.
//!
//! Twenty seven read, do something to what they read, and write it back. `__atomic_exchange_n` puts
//! a value there and answers what was there. Each of the six operations comes in four spellings, two
//! per family, and the two of a pair differ in whether they answer the value before or the value
//! after. `__sync_lock_test_and_set` and `__sync_lock_release` are the two halves of a lock, which
//! is an exchange and a store of a zero at the two orderings a lock needs.
//!
//! SQLite is why the first four and not some other four. Its `AtomicLoad` and `AtomicStore` macros
//! are `__atomic_load_n` and `__atomic_store_n` at relaxed ordering, it calls `__sync_synchronize`
//! directly twice, and those three names are the whole of what an amalgamation build asks for. The
//! two questions are here because glibc's headers ask them and because the answer is arithmetic
//! over two numbers, so the cost of having them is a page of reasons and eight lines of code. The
//! compare and exchange is here because it is the instruction every other atomic on this machine is
//! built out of, and the twenty seven are here because glibc and the kernel are written out of them:
//! a reference count is `__atomic_fetch_add`, a spin lock is the pair of lock names, and a flag set
//! in a word of them is `__atomic_fetch_or`.
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
//! `__atomic_load`, `__atomic_store` and `__atomic_exchange`, the forms that pass a value through a
//! second pointer rather than taking or answering one, which nothing measured uses.
//! `__atomic_test_and_set` and `__atomic_clear`, which are the pair of lock names over one byte and
//! go in beside them. And `__atomic_signal_fence`, which orders against a signal
//! handler on the same thread and so has to constrain the compiler while emitting no instruction at
//! all. The IR's `fence` is a machine barrier, so spelling a signal fence as one would be correct
//! and would cost an `mfence` that nothing needs. It waits for a barrier that says what it means.

use rucc_ast::UnaryOp;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{IntKind, layout, pointee};

use crate::check::Checker;
use crate::expr::{AtomicOp, Category, Expr, ExprId, ExprKind, Ordering, Rmw};
use crate::tast::Const;

/// The names that reach this through the type generic table, and what each one is.
///
/// `__sync_synchronize` is not here because it carries a signature, so it is checked as an ordinary
/// call and is answered by [`Checker::sync_builtin_value`] instead.
const FAMILY: &[(&str, AtomicOp)] = &[
    ("__atomic_load_n", AtomicOp::Load),
    ("__atomic_store_n", AtomicOp::Store),
    ("__atomic_thread_fence", AtomicOp::Fence),
    ("__atomic_compare_exchange_n", AtomicOp::CompareExchange),
    ("__atomic_compare_exchange", AtomicOp::CompareExchange),
    ("__sync_bool_compare_and_swap", AtomicOp::SwapBool),
    ("__sync_val_compare_and_swap", AtomicOp::SwapValue),
    ("__atomic_exchange_n", AtomicOp::Exchange),
    ("__atomic_fetch_add", AtomicOp::Fetch(Rmw::Add)),
    ("__atomic_fetch_sub", AtomicOp::Fetch(Rmw::Sub)),
    ("__atomic_add_fetch", AtomicOp::Update(Rmw::Add)),
    ("__atomic_sub_fetch", AtomicOp::Update(Rmw::Sub)),
    ("__sync_fetch_and_add", AtomicOp::Fetch(Rmw::Add)),
    ("__sync_fetch_and_sub", AtomicOp::Fetch(Rmw::Sub)),
    ("__sync_add_and_fetch", AtomicOp::Update(Rmw::Add)),
    ("__sync_sub_and_fetch", AtomicOp::Update(Rmw::Sub)),
    ("__sync_lock_test_and_set", AtomicOp::Exchange),
    ("__sync_lock_release", AtomicOp::Store),
    ("__atomic_fetch_and", AtomicOp::Fetch(Rmw::And)),
    ("__atomic_fetch_nand", AtomicOp::Fetch(Rmw::Nand)),
    ("__atomic_fetch_or", AtomicOp::Fetch(Rmw::Or)),
    ("__atomic_fetch_xor", AtomicOp::Fetch(Rmw::Xor)),
    ("__atomic_and_fetch", AtomicOp::Update(Rmw::And)),
    ("__atomic_nand_fetch", AtomicOp::Update(Rmw::Nand)),
    ("__atomic_or_fetch", AtomicOp::Update(Rmw::Or)),
    ("__atomic_xor_fetch", AtomicOp::Update(Rmw::Xor)),
    ("__sync_fetch_and_and", AtomicOp::Fetch(Rmw::And)),
    ("__sync_fetch_and_nand", AtomicOp::Fetch(Rmw::Nand)),
    ("__sync_fetch_and_or", AtomicOp::Fetch(Rmw::Or)),
    ("__sync_fetch_and_xor", AtomicOp::Fetch(Rmw::Xor)),
    ("__sync_and_and_fetch", AtomicOp::Update(Rmw::And)),
    ("__sync_nand_and_fetch", AtomicOp::Update(Rmw::Nand)),
    ("__sync_or_and_fetch", AtomicOp::Update(Rmw::Or)),
    ("__sync_xor_and_fetch", AtomicOp::Update(Rmw::Xor)),
];

/// The two names of the older family that are the two halves of a lock rather than a full barrier.
///
/// Everything else spelled `__sync_` orders everything against everything, and these two do not,
/// which is what gcc documents them as and is the whole reason they are spelled apart from
/// `__sync_lock_test_and_set`'s neighbours. Taking a lock has to keep what comes after it from
/// moving in front, and releasing one has to keep what came before it from moving out behind, and
/// neither has anything to say about the other direction.
const LOCK_TEST_AND_SET: &str = "__sync_lock_test_and_set";
const LOCK_RELEASE: &str = "__sync_lock_release";

/// The one of the two C11 compare and exchange names whose value to put there arrives by pointer.
///
/// The `_n` in the other one is the family's own mark for the form that takes a value, and the form
/// without it exists for an object too big to pass in a register. Both are here, and the difference
/// between them is one read, which is done where the name is still known so that everything below
/// sees the same shape.
const THROUGH_POINTER: &str = "__atomic_compare_exchange";

/// The one name of the older family that is not type generic.
const SYNCHRONIZE: &str = "__sync_synchronize";

/// The two names that ask about the target rather than about an object.
///
/// They are one answer here, which is a decision rather than an oversight and is written out where
/// [`Checker::lock_free_builtin_value`] answers them.
const LOCK_FREE: &[&str] = &["__atomic_always_lock_free", "__atomic_is_lock_free"];

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
///
/// A compare and exchange can be any of them, and so can a read modify write, because both read
/// and write and so have something to say about both directions. The ordering that holds when a
/// compare and exchange exchanged nothing is checked as a load's rather than here, since what the
/// operation did in that case is read the object and write nothing, which is a load.
fn allowed(op: AtomicOp, order: Ordering) -> bool {
    match op {
        AtomicOp::Load => matches!(order, Ordering::Relaxed | Ordering::Acquire | Ordering::SeqCst),
        AtomicOp::Store => {
            matches!(order, Ordering::Relaxed | Ordering::Release | Ordering::SeqCst)
        }
        AtomicOp::Fence
        | AtomicOp::CompareExchange
        | AtomicOp::SwapBool
        | AtomicOp::SwapValue
        | AtomicOp::Exchange
        | AtomicOp::Fetch(_)
        | AtomicOp::Update(_) => true,
    }
}

impl Checker<'_> {
    /// The node one of the type generic atomics becomes, once its arguments have been checked.
    ///
    /// The arguments arrive as values in the types they were written with. The ones that have to be
    /// converted are the values going into the object, which become the type of the object they are
    /// going into, because that is the width of the access and a store of a `char` through an
    /// `int *` is a four byte write.
    ///
    /// # What a compare and exchange drops
    ///
    /// Two of the C11 form's six arguments are checked and then thrown away, and both are thrown
    /// away because of what this machine is rather than because nobody got to them.
    ///
    /// The `weak` argument says the operation may fail when the object did hold the expected value,
    /// which lets a machine whose compare and exchange is a pair of linked instructions leave the
    /// retry loop to the caller. x86-64's is one instruction and never fails that way, so a weak
    /// compare and exchange and a strong one are the same instruction here and the argument decides
    /// nothing. gcc requires it to be a constant and this does not read it at all.
    ///
    /// The failure ordering says how strongly the operation is ordered when nothing was exchanged.
    /// It is checked, because a program that writes a release there has written something that is
    /// wrong everywhere, and then dropped, because the instruction this becomes is a locked one and
    /// a locked instruction on x86-64 is a full barrier whichever ordering was asked for.
    pub(in crate::check) fn atomic_builtin(
        &mut self,
        op: AtomicOp,
        spelled: &str,
        args: &[ExprId],
        span: Span,
    ) -> ExprId {
        let Some(order) = self.order_of(op, args, spelled) else { return self.poison(span) };

        let operands = match op {
            AtomicOp::Fence => Vec::new(),
            AtomicOp::Load => vec![args[0]],
            // The value goes in as the type of the object, which is the width of the access: a
            // store of a `char` through an `int *` writes four bytes.
            AtomicOp::Store | AtomicOp::Exchange | AtomicOp::Fetch(_) | AtomicOp::Update(_) => {
                let target = self.accessed(args[0]);
                vec![args[0], self.written(args.get(1).copied(), target, span)]
            }
            // The object, the place the value expected is, and the value to put there. The second
            // is a pointer in the C11 pair and a value in the older one, and it stays as written
            // rather than being made the same in both, because what is done with it differs: one
            // pair writes back through it and the other has nowhere to write back to.
            AtomicOp::CompareExchange => {
                let target = self.accessed(args[0]);
                let desired = if spelled == THROUGH_POINTER {
                    self.value_at(args[2], target, span)
                } else {
                    self.conv().to_type(args[2], target)
                };
                vec![args[0], args[1], desired]
            }
            AtomicOp::SwapBool | AtomicOp::SwapValue => {
                let target = self.accessed(args[0]);
                let expected = self.conv().to_type(args[1], target);
                let desired = self.conv().to_type(args[2], target);
                vec![args[0], expected, desired]
            }
        };
        let ty = match op {
            AtomicOp::Load
            | AtomicOp::SwapValue
            | AtomicOp::Exchange
            | AtomicOp::Fetch(_)
            | AtomicOp::Update(_) => self.accessed(args[0]),
            AtomicOp::CompareExchange | AtomicOp::SwapBool => self.types.boolean(),
            AtomicOp::Store | AtomicOp::Fence => self.types.void(),
        };
        let args = self.tast.add_expr_refs(&operands);
        self.tast.expr(Expr::new(ExprKind::Atomic { op, order, args }, ty, Category::Rvalue), span)
    }

    /// Which ordering the call asked for, out of however many orderings its name carries.
    ///
    /// One for the accesses, the barrier and the read modify writes of the C11 family, where it is
    /// the last argument, which is the shape that family has: the object comes first, whatever it is
    /// being handed comes next, and how strongly it is ordered comes last. Two for a compare and
    /// exchange, where the second is the one that holds when nothing was exchanged. None at all for
    /// the older family, which has no argument to say so with, and which of the two families a name
    /// is in is read off the spelling because that is exactly what it is.
    ///
    /// Nothing at all comes back when there is no argument where one was expected, which is a call
    /// that has already been complained about for its argument count.
    fn order_of(&mut self, op: AtomicOp, args: &[ExprId], spelled: &str) -> Option<Ordering> {
        // The trailing arguments of a `__sync_*` call are the variables it promises to protect, and
        // it protects them by being a full barrier, so there is nothing to read and nothing that
        // could have been written. The two halves of a lock are the exception and are weaker, which
        // is a thing gcc documents about them rather than a thing this works out.
        if spelled.starts_with("__sync_") {
            return Some(match spelled {
                LOCK_TEST_AND_SET => Ordering::Acquire,
                LOCK_RELEASE => Ordering::Release,
                _ => Ordering::SeqCst,
            });
        }
        if op == AtomicOp::CompareExchange {
            let [.., success, failure] = args else { return None };
            // Checked as a load's, because what the operation did when it exchanged nothing is read
            // the object and write nothing, which is a load. The answer is dropped: see above.
            let _ = self.ordering(AtomicOp::Load, *failure, spelled);
            return Some(self.ordering(op, *success, spelled));
        }
        let &written = args.last()?;
        Some(self.ordering(op, written, spelled))
    }

    /// The value going into the object, as the type of the object.
    ///
    /// Nothing was handed over for `__sync_lock_release`, which is the one write in the family whose
    /// value is not in the call: what it does is put a zero there, which is how a lock is given back
    /// whatever the object it is held in. The zero is written as an `int` and then converted rather
    /// than made in the object's type directly, so that an object that is a pointer gets the null
    /// pointer and one that is a `double` gets a floating zero, both of which are what that
    /// conversion is for.
    fn written(&mut self, value: Option<ExprId>, target: rucc_types::TypeId, span: Span) -> ExprId {
        let value = value.unwrap_or_else(|| {
            let int = self.types.int(IntKind::Int);
            self.constant(Const::Int(0), int, span)
        });
        self.conv().to_type(value, target)
    }

    /// The value at the end of a pointer the caller handed over, as an rvalue of the object's type.
    ///
    /// The argument has already been checked to be a pointer to the object type, by `argument_fits`
    /// in `check/builtin/generic.rs`, so the read is written here rather than going back through
    /// the checking of a `*` somebody typed.
    fn value_at(&mut self, pointer: ExprId, target: rucc_types::TypeId, span: Span) -> ExprId {
        let node = ExprKind::Unary { op: UnaryOp::Deref, operand: pointer };
        let read = self.tast.expr(Expr::new(node, target, Category::Lvalue), span);
        self.value(read)
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

    /// `__atomic_always_lock_free(size, p)` and `__atomic_is_lock_free(size, p)`, which are
    /// questions about the machine and answer as constants.
    ///
    /// Both take a size in bytes and a pointer that is there to say how the object is aligned, and
    /// both come back true when an object of that size and that alignment is one this compiler
    /// writes an instruction for rather than a call to a library. Which sizes those are is
    /// `lock_free_width` in `rucc_target::TargetInfo`, and it is eight bytes everywhere, so the
    /// answer here is that the size is one, two, four or eight and the object is aligned to at
    /// least its own size.
    ///
    /// # Why the two are one answer
    ///
    /// gcc separates them: the first has to be a constant and the second may become a call into
    /// libatomic, which decides at run time by looking at the address. There is no libatomic here
    /// and nothing to call, so a second answer would be a call to a function no object file
    /// defines. Folding both means a program that asks the second question gets the first
    /// question's answer, which is the stronger claim and so is never wrong where it says yes. The
    /// only thing a run time answer knows that this does not is what an address turned out to be
    /// aligned to, and nothing on this target does eight bytes atomically at one alignment and not
    /// at another, so there is no case where the second question has a better answer than this.
    ///
    /// # The size, and what a size that is not a constant means
    ///
    /// A size the compiler cannot fold answers no. It has to answer something, since the whole
    /// point of both names is that the answer is available before the program runs, and no is the
    /// answer that makes a program take the path that works whatever the size turns out to be. gcc
    /// refuses the first name outright in that case, and refusing here would break the header idiom
    /// these appear in, where the size is a macro that came out of some other platform's header.
    ///
    /// # The pointer
    ///
    /// A null pointer means the object has whatever alignment its type would naturally have, which
    /// is what gcc documents and is what every use in a header passes. Anything else is read for
    /// the type it points at, through the conversion to `const void *` that the prototype put
    /// there, since reading the argument where it stands would be asking a `void` how it is
    /// aligned. A pointer to something with no layout, which is a `void *` or an incomplete type,
    /// says nothing and is treated as the null pointer is.
    ///
    /// The alignment comes from the type and not from an analysis of the address, and gcc's comes
    /// from the address. So `__atomic_always_lock_free(4, &p->v)` where `v` is an `int` in a packed
    /// structure is yes here and no there: this sees an `int *` and gcc sees a field it laid out at
    /// an odd offset. The answer is not wrong on this machine, because the `lock` prefix works at
    /// any alignment on x86-64 and the operation really is lock free, and it would be wrong on a
    /// machine where it is not, so it is written down here rather than left for a target that has
    /// to care about it to discover.
    pub(in crate::check) fn lock_free_builtin_value(
        &mut self,
        function: Option<rucc_base::Symbol>,
        args: &[ExprId],
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__atomic_") || !LOCK_FREE.contains(&spelled) {
            return None;
        }
        let &[size, object] = args else { return None };
        let widest = u128::from(self.cx.target.lock_free_width / 8);
        let bytes = self.folded(size).and_then(|number| u128::try_from(number).ok());
        let free = bytes.is_some_and(|bytes| {
            bytes.is_power_of_two()
                && bytes <= widest
                && u128::from(self.aligned_to(object)) >= bytes
        });
        let boolean = self.types.boolean();
        Some(self.constant(Const::Int(i128::from(free)), boolean, span))
    }

    /// What that expression is as a number, or nothing if it is not one.
    ///
    /// The complaints folding made are dropped for the reason [`Checker::ordering`] drops them: a
    /// non constant argument is allowed in both places, and what folding says about one is that it
    /// is not a constant, which is not a complaint about this program.
    fn folded(&mut self, expr: ExprId) -> Option<i128> {
        if self.is_poisoned(expr) {
            return None;
        }
        let mut eval = self.eval();
        let folded = eval.constant(expr);
        let _ = eval.finish();
        match folded {
            Ok(Const::Int(number)) => Some(number),
            _ => None,
        }
    }

    /// What the object this pointer points at is aligned to, in bytes.
    ///
    /// Where nothing was said, which is the null pointer and the pointer to something with no
    /// layout, the answer is as large as it can be, so that the alignment stops being part of the
    /// question and the size decides it alone.
    fn aligned_to(&mut self, object: ExprId) -> u64 {
        if self.conv().is_null_pointer_constant(object) {
            return u64::MAX;
        }
        let mut expr = object;
        // Through the conversion the prototype put there, which is what holds the type that was
        // written. The parameter is `const void *` and a `void` has no alignment, so reading the
        // argument where it stands would answer nothing for every call.
        while let ExprKind::Cast(inner) | ExprKind::Convert { operand: inner, .. } =
            self.tast[expr].kind
        {
            expr = inner;
        }
        let Some(target) = pointee(&self.types, self.tast[expr].ty) else { return u64::MAX };
        layout(&self.types, target, self.cx.target).map_or(u64::MAX, |it| it.align)
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

    /// Every type generic name has to be a row of the roster that carries no signature, or the
    /// ordinary call checking would answer for it before this ever sees it.
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

    /// The name whose desired value arrives through a pointer is spelled out in one place and used
    /// in another, so the two are checked against each other rather than against a reader.
    #[test]
    fn the_name_that_takes_its_desired_value_through_a_pointer_is_one_of_the_family() {
        assert_eq!(shape(THROUGH_POINTER), Some(AtomicOp::CompareExchange));
        assert_eq!(THROUGH_POINTER, "__atomic_compare_exchange");
        assert_ne!(
            THROUGH_POINTER, "__atomic_compare_exchange_n",
            "the suffixed one takes a value"
        );
    }

    /// An exchange reads and writes, so unlike the two accesses there is no ordering it has nothing
    /// to say about, and a program that hands one a release is not to be turned away.
    #[test]
    fn an_exchange_takes_every_ordering_there_is() {
        for op in [AtomicOp::CompareExchange, AtomicOp::SwapBool, AtomicOp::SwapValue] {
            for &order in NUMBERED {
                assert!(allowed(op, order), "an exchange takes {order:?}");
            }
        }
    }

    /// The two questions carry a prototype for the reason the barrier does, which is what gets them
    /// to the place they are answered.
    #[test]
    fn the_two_questions_are_rows_that_carry_a_signature() {
        for &name in LOCK_FREE {
            let feature = rucc_gnu::lookup(Kind::Builtin, name).expect("a row of features.toml");
            assert_eq!(feature.status, Status::Implemented, "{name}");
            assert!(!feature.signature.is_empty(), "{name} is checked against its prototype");
            assert!(feature.library.is_empty(), "{name} is not a call to anything");
            assert!(shape(name).is_none(), "{name} is not one of the type generic ones");
        }
    }

    /// A name outside the family asks for nothing, including the ones spelled almost the same way
    /// that are the rest of tamnd/rucc#311.
    #[test]
    fn a_name_outside_the_family_asks_for_nothing() {
        assert_eq!(shape("__atomic_load_n"), Some(AtomicOp::Load));
        assert_eq!(shape("__atomic_store_n"), Some(AtomicOp::Store));
        assert_eq!(shape("__atomic_thread_fence"), Some(AtomicOp::Fence));
        assert_eq!(shape("__atomic_compare_exchange_n"), Some(AtomicOp::CompareExchange));
        assert_eq!(shape("__atomic_compare_exchange"), Some(AtomicOp::CompareExchange));
        assert_eq!(shape("__sync_bool_compare_and_swap"), Some(AtomicOp::SwapBool));
        assert_eq!(shape("__sync_val_compare_and_swap"), Some(AtomicOp::SwapValue));
        assert_eq!(shape("__atomic_exchange_n"), Some(AtomicOp::Exchange));
        assert_eq!(shape(LOCK_TEST_AND_SET), Some(AtomicOp::Exchange));
        assert_eq!(shape(LOCK_RELEASE), Some(AtomicOp::Store));
        assert_eq!(shape("__atomic_fetch_add"), Some(AtomicOp::Fetch(Rmw::Add)));
        assert_eq!(shape("__atomic_sub_fetch"), Some(AtomicOp::Update(Rmw::Sub)));
        assert_eq!(shape("__sync_fetch_and_sub"), Some(AtomicOp::Fetch(Rmw::Sub)));
        assert_eq!(shape("__sync_add_and_fetch"), Some(AtomicOp::Update(Rmw::Add)));
        assert_eq!(shape("__atomic_fetch_and"), Some(AtomicOp::Fetch(Rmw::And)));
        assert_eq!(shape("__atomic_nand_fetch"), Some(AtomicOp::Update(Rmw::Nand)));
        assert_eq!(shape("__sync_fetch_and_or"), Some(AtomicOp::Fetch(Rmw::Or)));
        assert_eq!(shape("__sync_xor_and_fetch"), Some(AtomicOp::Update(Rmw::Xor)));
        assert_eq!(shape("__atomic_load"), None);
        assert_eq!(shape("__atomic_store"), None);
        assert_eq!(shape("__atomic_exchange"), None);
        assert_eq!(shape("__atomic_signal_fence"), None);
        assert_eq!(shape("__atomic_test_and_set"), None);
        assert_eq!(shape("__atomic_clear"), None);
        assert_eq!(shape(SYNCHRONIZE), None);
    }
}
