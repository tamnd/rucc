//! `__builtin_object_size` and `__builtin_dynamic_object_size`, which ask how many bytes there
//! are behind an address.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! This is what `_FORTIFY_SOURCE` is built on. A fortified `string.h` turns every `memcpy` into
//! `__builtin___memcpy_chk(dst, src, len, __builtin_object_size(dst, 0))`, so a compiler with no
//! answer for this one has no answer for any copying function either, and every distribution
//! builds with the macro on. The `_chk` family is tamnd/rucc#225 and this is the half of it that
//! has to come first.
//!
//! # The four kinds
//!
//! The second argument is two bits. The low one says which object is being asked about: clear for
//! the whole object the address is inside, set for the closest member containing it. The high one
//! says which way to guess when there is more than one answer: clear for the largest and set for
//! the smallest. So `&s.b[2]` in a structure whose `b` is a twelve byte array ten bytes in asks
//! about `s` for kinds zero and two and about `b` for kinds one and three.
//!
//! Only the low bit does anything here, because nothing answered here is a guess. Where the object
//! is in front of us the largest and the smallest are the same number, and where it is not there
//! is no answer at all, which is `(size_t) -1` for the kinds that want the largest and zero for
//! the kinds that want the smallest. That pair is what the standard idiom in a fortified header
//! tests against, and answering it is what makes the header take the unchecked path.
//!
//! # What the walk sees
//!
//! A declared object, a string literal or a compound literal, with members and constant subscripts
//! on top of it and a constant displacement added to the whole. That is an address this compiler
//! knows the object of by looking at it, and no analysis is involved: every number comes from the
//! layout of a type. A dereference stops the walk, since what is behind `p->f` is a fact about
//! where `p` came from rather than about the expression, and so does an array with no size, which
//! is the flexible array member at the end of a structure.
//!
//! The storage duration does not matter. A local is as knowable as a global here, unlike in a
//! constant expression, where the difference is the whole question.
//!
//! # What is left for the IR
//!
//! An address this cannot see that is read out of a local of the function, with nothing in
//! working it out that a program could notice, is not answered here. It becomes an
//! `ExprKind::ObjectSize`, which lowers to an `object_size` instruction, and `rucc_opt::objsize`
//! answers it before any other pass, following the branches and loops that set the pointer. That
//! is where the guessing the high bit asks for happens. A pointer read out of a parameter or a
//! global is answered here as not known, because the IR has no more of where it came from, and
//! answering it now is what lets lowering make a checking call over one the plain call at `-O0`.
//!
//! # Where this parts company with gcc
//!
//! gcc gives up on the subobject question as soon as any pointer arithmetic is involved and this
//! does not, so `__builtin_object_size(g + 4, 1)` over a thirty two byte array is twenty eight
//! here and `(size_t) -1` at gcc 16.2.0's `-O0`. The two agree again at `-O1`, where gcc's own
//! object size pass runs and gives twenty eight, so what this differs from is a stage of gcc
//! rather than an answer of gcc's, and the number is the exact one either way. Measured on gcc
//! 16.2.0 over twelve shapes at `-O0` and `-O2`.
//!
//! Both compilers agree that the pointer is not evaluated. `__builtin_object_size(f(), 0)` does
//! not call `f`, which was measured rather than read off the manual, and it is the same rule
//! `sizeof` follows for the same reason: what the builtin reads is the shape of the expression and
//! not the value it would produce.
//!
//! One thing gcc refuses and this takes, which is either of these in a static initializer. gcc
//! folds them after the front end has decided what a constant expression is, so it reports
//! `initializer element is not constant`; here the answer is a constant by the time anything asks,
//! and refusing a program on the strength of when the folding happens to run is not a rule worth
//! copying.

use rucc_ast::{BinaryOp, UnaryOp};
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{ArrayLen, Qualifiers, TypeId, TypeKind, integer_info, layout};

use crate::check::Checker;
use crate::decl::{DeclKind, StorageDuration};
use crate::eval::bare;
use crate::expr::{Category, Conversion, Expr, ExprId, ExprKind};
use crate::tast::Const;

/// The two names, which ask the same question.
///
/// They differ in what a program may do with the answer rather than in what the answer is. The
/// dynamic spelling is allowed to come back as something computed while the program runs, so a
/// header may use it where the object is one that was allocated; the other must be a constant. A
/// constant satisfies both, so both are answered here and the same way.
const FAMILY: &[&str] = &["__builtin_object_size", "__builtin_dynamic_object_size"];

/// The code the second argument reports under.
const CODE: &str = "E0709";

/// The largest the second argument may be, which is the two bits it is made of.
const KINDS: i128 = 3;

/// An address whose object this compiler can see, and where in it the address lands.
#[derive(Debug, Clone, Copy)]
struct Reach {
    /// The size of the outermost object the address is inside.
    whole: u64,
    /// How far into that the address is.
    into_whole: u64,
    /// The size of the closest member or complete object containing the address.
    closest: u64,
    /// How far into that the address is.
    into_closest: u64,
}

impl Reach {
    /// An object whose own start this address is.
    const fn all(size: u64) -> Reach {
        Reach { whole: size, into_whole: 0, closest: size, into_closest: 0 }
    }

    /// A member `size` bytes long, reached by going `offset` bytes into whatever holds it.
    ///
    /// The member becomes the closest object and the outermost one is unchanged, which is the
    /// whole of the difference the low bit of the kind asks about.
    const fn member(self, offset: u64, size: u64) -> Reach {
        Reach {
            whole: self.whole,
            into_whole: self.into_whole.saturating_add(offset),
            closest: size,
            into_closest: 0,
        }
    }

    /// The same address moved along by `bytes`, which is what arithmetic on a pointer does.
    ///
    /// Nothing for a displacement that walks off the front, since an address before the object it
    /// was built from is one C says nothing about and a wrapped number is not an answer.
    fn moved(self, bytes: i128) -> Option<Reach> {
        let step = |from: u64| u64::try_from(i128::from(from).checked_add(bytes)?).ok();
        Some(Reach {
            into_whole: step(self.into_whole)?,
            into_closest: step(self.into_closest)?,
            ..self
        })
    }

    /// How many bytes are left in front of the address, which is the answer.
    ///
    /// Zero rather than a negative number for an address past the end of what it names. Such an
    /// address is either the one past the end C allows and has nothing in front of it, or it is
    /// one C says nothing about, and zero is the right answer to both.
    const fn left(self, closest: bool) -> u64 {
        let (size, into) =
            if closest { (self.closest, self.into_closest) } else { (self.whole, self.into_whole) };
        size.saturating_sub(into)
    }
}

impl Checker<'_> {
    /// The node for a call to either of the two, if the name is one of them.
    ///
    /// The name decides this rather than the declaration, the way it does for the rest of the
    /// family: what a name beginning `__builtin_` means is a fact about the name.
    pub(in crate::check) fn object_size_builtin(
        &mut self,
        function: Option<Symbol>,
        args: &[ExprId],
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_") || !FAMILY.contains(&spelled) {
            return None;
        }
        let spelled = spelled.to_owned();
        let &[address, kind] = args else { return None };
        if args.iter().any(|&arg| self.is_poisoned(arg)) {
            return Some(self.poison(span));
        }
        let Some(kind) = self.object_size_kind(&spelled, kind) else {
            return Some(self.poison(span));
        };
        let ty = self.size_type();
        let reach = self.behind(address);
        // Inside a function, an address read out of a local is asked about again once the function
        // is IR, where a pointer chosen by a branch or a loop is a block parameter whose every
        // argument is in front of the walk. Only where lowering the address does nothing a
        // program could notice, since the pointer is not evaluated, and never in a static
        // initializer, which has to be a constant by the time this is done. A pointer read out of
        // a global or a parameter came from somewhere the IR cannot see either, so it is answered
        // here and now, which is what lets a `_chk` call over one be the plain call at `-O0`.
        if reach.is_none() && self.body.is_some() && self.quiet(address) && self.local(address) {
            let kind = u8::try_from(kind).ok()?;
            let node = ExprKind::ObjectSize { address, kind };
            return Some(self.tast.expr(Expr::new(node, ty, Category::Rvalue), span));
        }
        let answer = match reach {
            Some(reach) => i128::from(reach.left(kind & 1 == 1)),
            // Nothing is known, so the answer is the one that says so. The two spellings of it
            // are the extremes of the range, because a kind asking for the largest has to name a
            // size no object is bigger than and one asking for the smallest has to name a size no
            // object is smaller than.
            None if kind & 2 == 0 => {
                integer_info(&self.types, ty, self.cx.target).map_or(-1, |info| info.wrap(-1))
            }
            None => 0,
        };
        Some(self.constant(Const::Int(answer), ty, span))
    }

    /// The second argument as one of the four kinds, or nothing with the complaint already made.
    ///
    /// gcc refuses a kind that is not a constant and a constant outside the range in the same
    /// sentence, and so does this, because both are the same defect: the builtin has to decide
    /// which of four questions it was asked before it can answer, and a number that is not there
    /// yet decides nothing.
    fn object_size_kind(&mut self, spelled: &str, kind: ExprId) -> Option<i128> {
        let span = self.tast.expr_span(kind);
        // The folding is asked and its own complaints are dropped, for the reason
        // `check/builtin/prefetch.rs` drops them: what is wrong here is that the argument is not a
        // constant, said in the words of the builtin it is an argument of.
        let value = self.eval().integer(kind).ok();
        if let Some(value) = value {
            if (0..=KINDS).contains(&value) {
                return Some(value);
            }
        }
        let what = format!("the second argument of `{spelled}` is a constant from 0 to {KINDS}");
        let note = "the low bit asks about the closest member rather than the whole object and \
                    the high bit asks for the smallest answer rather than the largest, so a \
                    number that is not known until the program runs names no question";
        self.report(Diagnostic::error(what, span).with_code(CODE).note(note, span));
        None
    }

    /// What the object behind this address is, or nothing when the expression does not say.
    ///
    /// The argument arrives converted to `const void *` by the prototype, so the conversions come
    /// off first: a `void` has no size, and what the call was written with is underneath them.
    fn behind(&mut self, address: ExprId) -> Option<Reach> {
        let mut expr = address;
        loop {
            match self.tast[expr].kind {
                ExprKind::Cast(inner) => expr = inner,
                ExprKind::Convert { kind, operand } if through(kind) => expr = operand,
                // The address of an object, which is the object.
                ExprKind::Unary { op: UnaryOp::AddrOf, operand } => return self.inside(operand),
                // A displacement from one, which is the same object further along. The scale comes
                // from what the pointer points at rather than from what the other side folded to,
                // since that is what decides the arithmetic.
                ExprKind::Binary { op: BinaryOp::Add, lhs, rhs } => {
                    return self.displaced(lhs, rhs, 1);
                }
                ExprKind::Binary { op: BinaryOp::Sub, lhs, rhs } => {
                    return self.displaced(lhs, rhs, -1);
                }
                // An array that decayed, which the loop has just walked through the conversion
                // of, and anything else that names an object rather than holding a pointer.
                _ => return self.inside(expr),
            }
        }
    }

    /// What the object this lvalue names is part of, or nothing when the walk cannot say.
    fn inside(&mut self, expr: ExprId) -> Option<Reach> {
        match self.tast[expr].kind {
            ExprKind::Decl(decl) => {
                // A parameter of array type is a pointer by the time it is declared, so what is
                // measured here is a pointer and not the array the program wrote, which is right:
                // nothing about the caller's object survives into the callee's type.
                Some(Reach::all(self.bytes_of(self.tast[decl].ty)?))
            }
            ExprKind::CompoundLiteral(decl) => Some(Reach::all(self.bytes_of(self.tast[decl].ty)?)),
            ExprKind::Str(_) => Some(Reach::all(self.bytes_of(self.tast[expr].ty)?)),
            ExprKind::Member { base, field } => {
                let reach = self.inside(base)?;
                let TypeKind::Record(record) = bare(&self.types, self.tast[base].ty) else {
                    return None;
                };
                let field = self.types.record_info(record).fields.get(field as usize).copied()?;
                // A bit-field has no address, so nothing may ask this about one, and the member
                // is skipped rather than measured in bytes it does not own.
                if field.bits.is_some() {
                    return None;
                }
                Some(reach.member(field.offset, self.bytes_of(field.ty)?))
            }
            ExprKind::Subscript { base, index } => {
                // The closest object is the array and not the element, which is gcc's rule and is
                // what makes `__builtin_object_size(&v.a[1], 1)` the rest of `a`.
                let count = self.eval().integer(index).ok()?;
                let array = self.decayed(base)?;
                let reach = self.inside(array)?;
                let element = i128::from(self.bytes_of(self.tast[expr].ty)?);
                let into = u64::try_from(count.checked_mul(element)?).ok()?;
                let whole = self.bytes_of(self.tast[array].ty)?;
                Some(Reach {
                    whole: reach.whole,
                    into_whole: reach.into_whole.saturating_add(into),
                    closest: whole,
                    into_closest: into,
                })
            }
            _ => None,
        }
    }

    /// Whether working this address out changes nothing, so that lowering it for the question
    /// the IR answers is the same as not evaluating it.
    ///
    /// Reads, members, subscripts, arithmetic and choices. A call, an assignment, an increment or
    /// anything else with an effect stops it, and so does a read of something `volatile`.
    fn quiet(&self, expr: ExprId) -> bool {
        if self.types.quals(self.tast[expr].ty).has(Qualifiers::VOLATILE) {
            return false;
        }
        match self.tast[expr].kind {
            ExprKind::Const(_) | ExprKind::Str(_) | ExprKind::Decl(_) => true,
            ExprKind::Cast(inner) | ExprKind::Convert { operand: inner, .. } => self.quiet(inner),
            ExprKind::Member { base, .. } => self.quiet(base),
            ExprKind::Unary { op, operand } => {
                !matches!(
                    op,
                    UnaryOp::PreInc | UnaryOp::PreDec | UnaryOp::PostInc | UnaryOp::PostDec
                ) && self.quiet(operand)
            }
            ExprKind::Subscript { base: lhs, index: rhs } | ExprKind::Binary { lhs, rhs, .. } => {
                self.quiet(lhs) && self.quiet(rhs)
            }
            ExprKind::Cond { cond, then, otherwise } => {
                self.quiet(cond) && self.quiet(then) && self.quiet(otherwise)
            }
            _ => false,
        }
    }

    /// Whether the address reads a local of the function being checked, which is the one place a
    /// pointer can come from that the IR has every assignment of in front of it.
    fn local(&self, expr: ExprId) -> bool {
        match self.tast[expr].kind {
            ExprKind::Decl(id) => {
                let decl = &self.tast[id];
                decl.kind == DeclKind::Object
                    && decl.duration == StorageDuration::Automatic
                    && !self.is_parameter(id)
            }
            ExprKind::Cast(inner) | ExprKind::Convert { operand: inner, .. } => self.local(inner),
            ExprKind::Binary { lhs, rhs, .. } => self.local(lhs) || self.local(rhs),
            ExprKind::Cond { then, otherwise, .. } => self.local(then) || self.local(otherwise),
            _ => false,
        }
    }

    /// The lvalue an array to pointer conversion was applied to, if that is what this is.
    ///
    /// A subscript whose base is a real pointer rather than a decayed array stops the walk, since
    /// where that pointer came from is not a fact about the expression.
    fn decayed(&mut self, base: ExprId) -> Option<ExprId> {
        let mut expr = base;
        loop {
            match self.tast[expr].kind {
                ExprKind::Cast(inner) => expr = inner,
                ExprKind::Convert { kind, operand } if through(kind) => expr = operand,
                _ => break,
            }
        }
        matches!(bare(&self.types, self.tast[expr].ty), TypeKind::Array { .. }).then_some(expr)
    }

    /// The object behind `lhs op rhs`, where one side is the pointer and the other is a count.
    ///
    /// Either side may be the pointer, since `4 + p` is the same address as `p + 4`. The sign is
    /// what the operator contributes, and a subtraction whose right side is the pointer never
    /// arrives here, because the difference of two pointers is an integer.
    fn displaced(&mut self, lhs: ExprId, rhs: ExprId, sign: i128) -> Option<Reach> {
        let (pointer, count) = if let Some(step) = self.step_of(lhs) {
            (lhs, i128::from(step).checked_mul(self.eval().integer(rhs).ok()?)?)
        } else {
            let step = self.step_of(rhs)?;
            (rhs, i128::from(step).checked_mul(self.eval().integer(lhs).ok()?)?)
        };
        self.behind(pointer)?.moved(sign.checked_mul(count)?)
    }

    /// The size of what this pointer or array points at, which is what its arithmetic is scaled
    /// by.
    fn step_of(&mut self, expr: ExprId) -> Option<u64> {
        let target = match bare(&self.types, self.tast[expr].ty) {
            TypeKind::Pointer(target) | TypeKind::Array { elem: target, .. } => target,
            _ => return None,
        };
        self.bytes_of(target)
    }

    /// How large an object of this type is, or nothing when it has no size.
    ///
    /// The one that matters is the array with no length at the end of a structure, which is a
    /// promise that more bytes follow and no claim about how many, so a program asking about it
    /// gets the answer that says nothing.
    fn bytes_of(&mut self, ty: TypeId) -> Option<u64> {
        if let TypeKind::Array { len: ArrayLen::Unknown | ArrayLen::Star, .. } =
            bare(&self.types, ty)
        {
            return None;
        }
        layout(&self.types, ty, self.cx.target).ok().map(|it| it.size)
    }
}

/// Whether the walk can see through this conversion to the object underneath it.
///
/// The one that is not transparent is the read, and it is the whole of the difference between an
/// address this compiler knows the object of and one it does not. `g` where `g` is an array is the
/// array itself under a decay, so the walk goes through it; `p` where `p` is a pointer is a read
/// of a variable, and what was stored in it is a fact about the program's history rather than
/// about the expression, so the walk stops there and the answer is the one that says nothing.
const fn through(kind: Conversion) -> bool {
    match kind {
        Conversion::ArrayDecay
        | Conversion::FunctionDecay
        | Conversion::Pointer
        | Conversion::NullPointer
        | Conversion::Void => true,
        Conversion::Lvalue | Conversion::Arithmetic | Conversion::Bool | Conversion::Broadcast => {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Both names are rows of the table that carry a signature, because the signature is what the
    /// call is checked against before this replaces it. A row without one would never be declared
    /// and the call would be to an undeclared name.
    #[test]
    fn both_names_are_rows_of_the_table_that_carry_the_same_signature() {
        for name in FAMILY {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented);
            assert_eq!(feature.signature, "size_t(const void *, int)", "{name}");
            assert!(feature.library.is_empty(), "{name} is not a call to anything");
        }
    }

    /// An address past the end of what it names has nothing in front of it, rather than a number
    /// that wrapped.
    ///
    /// One past the end is an address C allows a program to form and not to read, and anything
    /// further is one C says nothing about, so zero is the right answer to both. A subtraction is
    /// what saturates here instead, and letting it wrap would hand a fortified header the largest
    /// size there is for the one address it should refuse.
    #[test]
    fn an_address_past_the_end_has_nothing_left_in_front_of_it() {
        let reach = Reach::all(8).moved(8).expect("one past the end is an address");
        assert_eq!(reach.left(false), 0);
        assert_eq!(reach.left(true), 0);
        let reach = Reach::all(8).moved(40).expect("further is still an address");
        assert_eq!(reach.left(false), 0);
    }

    /// An address before the object it was built from has no answer at all.
    ///
    /// It is not an address C gives a meaning to, and the arithmetic that would produce a size for
    /// it is the arithmetic that wraps. Nothing is what the caller turns into the unknown answer,
    /// which is the one a fortified header reads as a copy it cannot check.
    #[test]
    fn an_address_in_front_of_its_object_is_not_one_this_answers_about() {
        assert!(Reach::all(8).moved(-1).is_none());
    }

    /// The low bit of the kind is the whole of the difference between the two questions, and the
    /// member is what the narrower one names.
    ///
    /// Worth a test of its own because the two answers coincide for every object that is not
    /// inside another one, which is most of them, so a walk that ignored the bit would look right
    /// almost everywhere and be wrong exactly where `_FORTIFY_SOURCE` is most useful.
    #[test]
    fn the_low_bit_of_the_kind_picks_the_member_out_of_the_object_it_is_in() {
        let reach = Reach::all(24).member(12, 12).moved(2).expect("two bytes into a member");
        assert_eq!(reach.left(false), 10, "twenty four bytes with fourteen used");
        assert_eq!(reach.left(true), 10, "twelve bytes with two used");
        let reach = Reach::all(24).member(0, 8);
        assert_eq!(reach.left(false), 24);
        assert_eq!(reach.left(true), 8);
    }
}
