//! The builtins whose type comes from the call rather than from a table.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! `__atomic_load_n(p, order)` gives an `int` back when `p` is an `int *` and a `long` when it is
//! a `long *`. There is no one type to declare it as, so `features.toml` has no signature for it
//! and the file next door cannot answer for it. gcc calls these type generic, and what stands in
//! for a signature is a rule about the shape of the call: the first argument is a pointer, what
//! it points at is the type of the value arguments and usually of the answer, and the rest are
//! memory orders and flags whose types are fixed.
//!
//! That rule is [`GENERIC`], one row per name, and everything below it turns a row and a list of
//! arguments into an ordinary prototype. Once the prototype exists the call is checked like any
//! other call, which is the point: a wrong argument gets the same message here as it would to a
//! function the program declared, and none of the rules about conversions are written twice.
//!
//! # Why the declaration is thrown away
//!
//! `check/builtin.rs` declares a builtin once and leaves it in the file scope, because the type
//! it worked out will be the same the next time. Here it will not be. A file that calls
//! `__atomic_load_n` on an `int *` and again on a `long *` has two calls with two types and no
//! declaration that fits both, so each call builds its own and puts it in no scope at all. Every
//! call is decided on its own, which is what type generic means.
//!
//! # What is not here
//!
//! Anything the call then does. This gets the program past the type checker; the call still
//! reaches the IR as a call to a name no object file defines, exactly as the ordinary builtins
//! do, and every row in the table stays `unimplemented` until the lowering lands. What that
//! unblocks in the meantime is the headers: glibc's `atomic.h` and `stdatomic.h` are written
//! out of this family, and until it is accepted nothing that includes them is read at all.
//!
//! The `__sync_*` family's promise that the barrier protects a named list of variables. The
//! trailing arguments are accepted and given the default promotions, which is what gcc does with
//! them on every target that has a full barrier anyway.

use rucc_ast as ast;
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{
    FunctionType, IntKind, TypeId, TypeKind, is_integer, is_pointer, is_void, pointee,
};

use crate::check::Checker;
use crate::decl::{Decl, DeclKind, DeclList, Definition, Linkage, StorageDuration};
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::tast::{Base, Const};

/// What one argument of a type generic builtin is.
///
/// The set is small because the family is regular. Nearly every atomic takes a pointer, some
/// values of what it points at, and a memory order, and the two that are not shaped like that
/// are the ones this has extra variants for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Param {
    /// The pointer to the object the builtin works on, which is always the first argument and is
    /// what every other type in the call is worked out from.
    Object,
    /// A value of the type the object points at.
    Value,
    /// A pointer to a value of that type, which is where the builtin reads one from or writes
    /// one to instead of taking or answering with it.
    Place,
    /// A memory order, which gcc takes as an `int` and not as an enumeration, so that a program
    /// may write the number.
    Order,
    /// Whether a compare exchange is allowed to fail when it did not have to.
    Weak,
    /// An integer of whatever type it was written as, which is what the overflow builtins take
    /// and is not worked out from anything else.
    Integer,
    /// A pointer to an integer the answer is written through.
    Out,
    /// Whatever it was handed, which is what a builtin that reads the type of its argument
    /// rather than its value takes.
    Any,
}

/// What the call answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// What the first argument points at, with the qualifiers and the `_Atomic` off it.
    Pointee,
    /// Nothing.
    Void,
    /// Yes or no, which gcc gives as `_Bool`.
    Bool,
    /// An `int`, which is what the two builtins that classify their argument answer with.
    Int,
}

/// One name and the rule that stands in for its signature.
#[derive(Debug, Clone, Copy)]
struct Generic {
    /// The name, spelled the way the program writes it.
    name: &'static str,
    /// What the arguments are, in order.
    params: &'static [Param],
    /// What the call answers with.
    answer: Answer,
    /// Whether arguments past the ones named are allowed. Only the `__sync_*` family takes any,
    /// where they are the variables the barrier is promised to protect.
    trailing: bool,
}

impl Generic {
    /// Whether anything in the call is built out of what the first argument points at.
    ///
    /// A `void *` is enough for `__atomic_test_and_set`, which only wants somewhere to write a
    /// byte, and is not enough for `__atomic_load_n`, which has to answer with something.
    fn needs_pointee(&self) -> bool {
        self.answer == Answer::Pointee
            || self.params.iter().any(|param| matches!(param, Param::Value | Param::Place))
    }
}

/// A pointer, a value of what it points at, and an order, which is most of the atomic family.
const RMW: &[Param] = &[Param::Object, Param::Value, Param::Order];

/// Two integers and somewhere to put the result, which is the overflow family.
const OVERFLOW: &[Param] = &[Param::Integer, Param::Integer, Param::Out];

/// A pointer and a value of what it points at, which is most of the older `__sync_*` family.
const SYNC: &[Param] = &[Param::Object, Param::Value];

/// One row, written short because there are forty of them and the shape is the whole content.
const fn row(name: &'static str, params: &'static [Param], answer: Answer) -> Generic {
    Generic { name, params, answer, trailing: false }
}

/// A `__sync_*` row, which takes the list of protected variables after the arguments it uses.
const fn sync(name: &'static str, params: &'static [Param], answer: Answer) -> Generic {
    Generic { name, params, answer, trailing: true }
}

/// Every builtin whose type comes from the call.
///
/// The names are also rows of `features.toml`, which is the roster, and the test at the bottom
/// of this file is what keeps the two from drifting. The roster says which builtins exist and
/// what `__has_builtin` answers about them; this says how to check a call to one.
const GENERIC: &[Generic] = &[
    // The C11 model, from gcc 4.7. The `_n` in a name is what tells the form that takes a value
    // from the form that takes a pointer to one, which is how the family handles a type too big
    // to pass in a register.
    row("__atomic_load_n", &[Param::Object, Param::Order], Answer::Pointee),
    row("__atomic_load", &[Param::Object, Param::Place, Param::Order], Answer::Void),
    row("__atomic_store_n", RMW, Answer::Void),
    row("__atomic_store", &[Param::Object, Param::Place, Param::Order], Answer::Void),
    row("__atomic_exchange_n", RMW, Answer::Pointee),
    row(
        "__atomic_exchange",
        &[Param::Object, Param::Place, Param::Place, Param::Order],
        Answer::Void,
    ),
    row(
        "__atomic_compare_exchange_n",
        &[Param::Object, Param::Place, Param::Value, Param::Weak, Param::Order, Param::Order],
        Answer::Bool,
    ),
    row(
        "__atomic_compare_exchange",
        &[Param::Object, Param::Place, Param::Place, Param::Weak, Param::Order, Param::Order],
        Answer::Bool,
    ),
    row("__atomic_fetch_add", RMW, Answer::Pointee),
    row("__atomic_fetch_sub", RMW, Answer::Pointee),
    row("__atomic_fetch_and", RMW, Answer::Pointee),
    row("__atomic_fetch_or", RMW, Answer::Pointee),
    row("__atomic_fetch_xor", RMW, Answer::Pointee),
    row("__atomic_fetch_nand", RMW, Answer::Pointee),
    row("__atomic_add_fetch", RMW, Answer::Pointee),
    row("__atomic_sub_fetch", RMW, Answer::Pointee),
    row("__atomic_and_fetch", RMW, Answer::Pointee),
    row("__atomic_or_fetch", RMW, Answer::Pointee),
    row("__atomic_xor_fetch", RMW, Answer::Pointee),
    row("__atomic_nand_fetch", RMW, Answer::Pointee),
    row("__atomic_test_and_set", &[Param::Object, Param::Order], Answer::Bool),
    row("__atomic_clear", &[Param::Object, Param::Order], Answer::Void),
    row("__atomic_thread_fence", &[Param::Order], Answer::Void),
    row("__atomic_signal_fence", &[Param::Order], Answer::Void),
    // The older family, from gcc 4.1, which is still what sqlite and a good deal of the kernel
    // are written out of.
    sync("__sync_fetch_and_add", SYNC, Answer::Pointee),
    sync("__sync_fetch_and_sub", SYNC, Answer::Pointee),
    sync("__sync_fetch_and_or", SYNC, Answer::Pointee),
    sync("__sync_fetch_and_and", SYNC, Answer::Pointee),
    sync("__sync_fetch_and_xor", SYNC, Answer::Pointee),
    sync("__sync_fetch_and_nand", SYNC, Answer::Pointee),
    sync("__sync_add_and_fetch", SYNC, Answer::Pointee),
    sync("__sync_sub_and_fetch", SYNC, Answer::Pointee),
    sync("__sync_or_and_fetch", SYNC, Answer::Pointee),
    sync("__sync_and_and_fetch", SYNC, Answer::Pointee),
    sync("__sync_xor_and_fetch", SYNC, Answer::Pointee),
    sync("__sync_nand_and_fetch", SYNC, Answer::Pointee),
    sync(
        "__sync_bool_compare_and_swap",
        &[Param::Object, Param::Value, Param::Value],
        Answer::Bool,
    ),
    sync(
        "__sync_val_compare_and_swap",
        &[Param::Object, Param::Value, Param::Value],
        Answer::Pointee,
    ),
    sync("__sync_lock_test_and_set", SYNC, Answer::Pointee),
    sync("__sync_lock_release", &[Param::Object], Answer::Void),
    // The rest, which are type generic for a different reason: they take an argument in order to
    // ask something about its type, and answer with a number whatever it was.
    row("__builtin_add_overflow", OVERFLOW, Answer::Bool),
    row("__builtin_sub_overflow", OVERFLOW, Answer::Bool),
    row("__builtin_mul_overflow", OVERFLOW, Answer::Bool),
    row(CONSTANT_P, &[Param::Any], Answer::Int),
    row("__builtin_classify_type", &[Param::Any], Answer::Int),
];

/// `__builtin_constant_p`, the one row here that is answered rather than called.
const CONSTANT_P: &str = "__builtin_constant_p";

impl Checker<'_> {
    /// Checks a call to a builtin whose type comes from the call, if the name is one.
    ///
    /// Answers nothing when the name is not in the table, or when the program declared it
    /// itself, in which case what it declared is what the call is checked against and this has
    /// no business overriding it.
    pub(in crate::check) fn generic_builtin_call(
        &mut self,
        name: Symbol,
        args: ast::ExprList,
        span: Span,
    ) -> Option<ExprId> {
        let spelled = self.text(name);
        // Every name here starts with two underscores and every other called name in the program
        // reaches this too. The test costs a byte and saves the search.
        if !spelled.starts_with("__") {
            return None;
        }
        // A program that declared the name itself gets what it declared, whichever family the
        // name would otherwise have been in. A declaration this compiler made for the program is
        // not one of those, or the first call to a builtin would decide what the second means.
        if self.scopes.lookup(name).is_some() && !self.declared_builtins.contains(&name) {
            return None;
        }
        // The floating point classification family, which is type generic for the same reason and
        // is answered rather than called. In `check/builtin/classify.rs`, with why.
        if let Some(answer) = self.classify_builtin_call(name, args, span) {
            return Some(answer);
        }
        // The family whose answer is a constant, which is what a static initializer written with
        // one needs. In `check/builtin/constant.rs`, along with when it does not fold.
        if let Some(answer) = self.constant_builtin_call(name, args, span) {
            return Some(answer);
        }
        let spelled = self.text(name);
        let generic = *GENERIC.iter().find(|row| row.name == spelled)?;
        if generic.name == CONSTANT_P {
            return Some(self.constant_p(args, span));
        }
        Some(self.generic_call(name, generic, args, span))
    }

    /// `__builtin_constant_p(x)`, which is answered here and never reaches the IR.
    ///
    /// gcc folds it after optimization, so the answer for an argument that is not written as a
    /// constant can differ between `-O0` and `-O2`: a static function whose parameter is five at
    /// its one call site answers one once the call has been inlined and zero before that. This
    /// answers what the front end can see, which is the same answer at every optimization level
    /// and is the one every small compiler gives. It is also the answer glibc's headers are
    /// written against, since what they use it for is choosing between a version that needs a
    /// literal and one that does not.
    ///
    /// The argument is checked and then dropped. Nothing evaluates it, which is what gcc does
    /// with it as well: `__builtin_constant_p(i++)` leaves `i` alone.
    fn constant_p(&mut self, args: ast::ExprList, span: Span) -> ExprId {
        let written: Vec<ast::ExprId> = self.ast[args].to_vec();
        let [written] = written[..] else {
            let how = if written.is_empty() { "few" } else { "many" };
            self.report(
                Diagnostic::error(format!("too {how} arguments to function '{CONSTANT_P}'"), span)
                    .with_code("E0511"),
            );
            return self.poison(span);
        };
        let arg = self.expr(written);
        let arg = self.value(arg);
        let answer = !self.is_poisoned(arg) && self.folds(arg);
        let int = self.int();
        self.constant(Const::Int(i128::from(answer)), int, span)
    }

    /// Whether an expression folds to something gcc counts as a constant.
    ///
    /// Whatever the folding would have said is dropped. An argument that is not a constant is
    /// the question rather than a mistake, so a message about it would be a message about the
    /// program having asked.
    ///
    /// A string literal counts and the address of an object does not, which is measured on gcc
    /// 16 rather than reasoned about: `__builtin_constant_p("abc")` is one and
    /// `__builtin_constant_p(&g)` is zero, at every optimization level.
    fn folds(&mut self, arg: ExprId) -> bool {
        let mut eval = self.eval();
        let value = eval.constant(arg);
        let _ = eval.finish();
        match value {
            Ok(Const::Int(_) | Const::Float(_)) => true,
            Ok(Const::Address(address)) => matches!(address.base, Base::Str(_)),
            Err(_) => false,
        }
    }

    /// The call itself, once the name has been recognised.
    fn generic_call(
        &mut self,
        name: Symbol,
        generic: Generic,
        args: ast::ExprList,
        span: Span,
    ) -> ExprId {
        let written: Vec<ast::ExprId> = self.ast[args].to_vec();
        // The arguments are checked before anything else, which is the opposite of an ordinary
        // call and is the whole reason this is a separate path: the prototype is built out of
        // what they turned out to be, so it cannot exist until they have been checked.
        let checked: Vec<ExprId> = written
            .into_iter()
            .map(|arg| {
                let arg = self.expr(arg);
                self.value(arg)
            })
            .collect();
        let spelled = self.text(name).to_owned();

        let wanted = generic.params.len();
        let given = checked.len();
        if given < wanted || (given > wanted && !generic.trailing) {
            let how = if given < wanted { "few" } else { "many" };
            self.report(
                Diagnostic::error(format!("too {how} arguments to function '{spelled}'"), span)
                    .with_code("E0511"),
            );
            return self.poison(span);
        }

        let Some(target) = self.object_type(&generic, &checked, &spelled) else {
            return self.poison(span);
        };

        let mut params = Vec::with_capacity(wanted);
        let mut wrong = false;
        for (index, &param) in generic.params.iter().enumerate() {
            let arg = checked[index];
            let ty = self.tast[arg].ty;
            // A poisoned argument has already been complained about and its type says nothing,
            // so it is taken as it is and nothing further is said about it.
            if !self.is_poisoned(arg) && !self.argument_fits(param, arg, index, &spelled) {
                wrong = true;
            }
            params.push(match param {
                Param::Value => target.unwrap_or(ty),
                Param::Place => match target {
                    Some(target) => self.types.pointer(target),
                    None => ty,
                },
                Param::Order => self.types.int(IntKind::Int),
                Param::Weak => self.types.boolean(),
                Param::Object | Param::Integer | Param::Out | Param::Any => ty,
            });
        }
        if wrong {
            return self.poison(span);
        }

        let ret = match generic.answer {
            Answer::Pointee => target.unwrap_or_else(|| self.types.void()),
            Answer::Void => self.types.void(),
            Answer::Bool => self.types.boolean(),
            Answer::Int => self.types.int(IntKind::Int),
        };
        let ty = self.types.function(FunctionType {
            ret,
            params,
            variadic: generic.trailing,
            prototyped: true,
        });
        // In no scope, for the reason at the top of this file: the next call to the same name may
        // want a different type, and a declaration left behind would be the wrong one.
        let decl = self.tast.decl(
            Decl {
                name: Some(name),
                ty,
                kind: DeclKind::Function,
                linkage: Linkage::External,
                duration: StorageDuration::Static,
                state: Definition::Declared,
                alignment: None,
                constant: false,
                init: None,
                params: DeclList::EMPTY,
                body: None,
            },
            span,
        );
        let callee = self.tast.expr(Expr::new(ExprKind::Decl(decl), ty, Category::Function), span);
        let callee = self.value(callee);
        self.finish_call(callee, Some(name), checked, span)
    }

    /// What the first argument points at, once the qualifiers and the `_Atomic` are off it.
    ///
    /// Answers `Some(None)` for a builtin that has no such argument, and nothing at all when the
    /// argument is there and is not something to work from, having said so.
    fn object_type(
        &mut self,
        generic: &Generic,
        checked: &[ExprId],
        spelled: &str,
    ) -> Option<Option<TypeId>> {
        if generic.params.first() != Some(&Param::Object) {
            return Some(None);
        }
        let arg = checked[0];
        if self.is_poisoned(arg) {
            return None;
        }
        let ty = self.tast[arg].ty;
        let at = self.tast.expr_span(arg);
        let target = pointee(&self.types, ty)
            .filter(|&target| !generic.needs_pointee() || !is_void(&self.types, target));
        let Some(target) = target else {
            self.report(
                Diagnostic::error(
                    format!("argument 1 of '{spelled}' must be a non-void pointer type"),
                    at,
                )
                .with_code("E0670"),
            );
            return None;
        };
        Some(Some(self.plain(target)))
    }

    /// Whether one argument is of a shape the builtin can take, having said so if it is not.
    ///
    /// Only the arguments whose type is a constraint of the builtin rather than a conversion are
    /// asked about here. A value of the wrong arithmetic type converts and is not this
    /// function's business, because the prototype is what handles that.
    fn argument_fits(&mut self, param: Param, arg: ExprId, index: usize, spelled: &str) -> bool {
        let ty = self.tast[arg].ty;
        let at = self.tast.expr_span(arg);
        let number = index + 1;
        match param {
            Param::Place if !is_pointer(&self.types, ty) => {
                self.report(
                    Diagnostic::error(
                        format!("argument {number} of '{spelled}' must be a pointer type"),
                        at,
                    )
                    .with_code("E0670"),
                );
                false
            }
            // The wording is gcc's, which names the call rather than the function for this family
            // because these are the builtins a macro most often expands to.
            Param::Integer if !is_integer(&self.types, ty) => {
                self.report(
                    Diagnostic::error(
                        format!(
                            "argument {number} in call to function '{spelled}' does not have \
                             integral type"
                        ),
                        at,
                    )
                    .with_code("E0671"),
                );
                false
            }
            Param::Out
                if !pointee(&self.types, ty)
                    .is_some_and(|target| is_integer(&self.types, target)) =>
            {
                self.report(
                    Diagnostic::error(
                        format!(
                            "argument {number} in call to function '{spelled}' does not have \
                             pointer to integral type"
                        ),
                        at,
                    )
                    .with_code("E0671"),
                );
                false
            }
            _ => true,
        }
    }

    /// A type with the qualifiers and the `_Atomic` taken off it.
    ///
    /// What comes out of an atomic object is an ordinary value of the ordinary type: reading a
    /// `_Atomic const int` gives an `int`, and leaving either of those on would make the answer
    /// something a program cannot assign to anything.
    fn plain(&mut self, ty: TypeId) -> TypeId {
        let inner = match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Atomic(inner) => inner,
            _ => ty,
        };
        self.types.unqualified(inner)
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::Kind;

    use super::*;

    /// The roster and the rules have to name the same builtins. A name here that is not a row of
    /// `features.toml` is one `__has_builtin` has never heard of, and a builtin there with
    /// neither a signature nor a rule here is one every call to is an undeclared name.
    #[test]
    fn every_rule_is_a_row_of_the_table_and_every_row_without_a_signature_has_a_rule() {
        for generic in GENERIC {
            let feature = rucc_gnu::lookup(Kind::Builtin, generic.name);
            assert!(feature.is_some(), "{} has a rule and is not in features.toml", generic.name);
        }
        // The builtins that are syntax rather than functions. Each is a keyword, so a call to one
        // never reaches a name lookup at all and neither table has anything to say about it.
        let syntax = [
            "__builtin_types_compatible_p",
            "__builtin_choose_expr",
            "__builtin_offsetof",
            "__builtin_va_list",
            "__builtin_va_start",
            "__builtin_va_arg",
            "__builtin_va_end",
            "__builtin_va_copy",
        ];
        for feature in rucc_gnu::features() {
            if feature.kind != Kind::Builtin || !feature.signature.is_empty() {
                continue;
            }
            let known = GENERIC.iter().any(|generic| generic.name == feature.name)
                || crate::check::builtin::classify::is_family(feature.name)
                || syntax.contains(&feature.name);
            assert!(known, "{} has neither a signature nor a rule", feature.name);
        }
    }

    /// A rule that takes values of what the object points at has to have an object to point at,
    /// or there is nothing to build the rest of the call out of.
    #[test]
    fn a_rule_that_uses_the_object_asks_for_one_first() {
        for generic in GENERIC {
            let uses = generic.needs_pointee();
            let has = generic.params.first() == Some(&Param::Object);
            assert!(!uses || has, "{} uses what it was not given", generic.name);
            let others = generic.params.iter().skip(1).filter(|p| **p == Param::Object).count();
            assert_eq!(others, 0, "{} names an object twice", generic.name);
        }
    }
}
