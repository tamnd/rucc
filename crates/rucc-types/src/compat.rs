//! Type compatibility and the composite type.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.2.
//!
//! Compatibility, 6.2.7, is the looser relation that identity is not. Two types are the same
//! when they are the same id, which is what the interner is for; two types are compatible when
//! C says a declaration of one may follow a declaration of the other. `int f(int a[3])` and
//! `int f(int *a)` declare the same function, an `enum` is compatible with whatever it is
//! represented in, and an array with a size is compatible with one without.
//!
//! The composite type, 6.2.7p3, is what a redeclaration leaves behind: the type that takes the
//! array size from whichever declaration had one and the parameter list from whichever
//! declaration was a prototype. `extern int a[]; int a[4];` has to end up with an array of
//! four, and a compiler that keeps the first type instead has lost the size for good.
//!
//! The rules here were checked by writing each pair of declarations and seeing which ones gcc
//! 13.3 and clang 18 refuse. They agree everywhere except one place, recorded on
//! [`records`]: clang implements the C23 rule that an identical structure redefinition in one
//! translation unit is the same type, and gcc 13.3 still rejects it.
//!
//! Nothing in here needs the target. Everything target dependent about a type has already been
//! decided by the time it is in the table: an enumeration knows what it is represented in and
//! an array knows how many elements it has.

use crate::kind::{ArrayLen, FloatKind, FunctionType, IntKind, RecordId, TypeKind};
use crate::types::{TypeId, Types};

/// Whether a declaration of `left` and a declaration of `right` declare the same thing, 6.2.7.
///
/// Identity implies compatibility and is one integer comparison, so this only does any work
/// when the two ids differ.
#[must_use]
pub fn compatible(types: &Types, left: TypeId, right: TypeId) -> bool {
    let mut assumed = Vec::new();
    same(types, left, right, &mut assumed)
}

/// The composite type of two compatible types, 6.2.7p3, and [`None`] when they are not
/// compatible.
///
/// It takes whatever each side knows: the size from the declaration that had one, the parameter
/// list from the declaration that was a prototype. This is what a caller merging two
/// declarations of one name should store, rather than either type it was given.
pub fn composite(types: &mut Types, left: TypeId, right: TypeId) -> Option<TypeId> {
    if !compatible(types, left, right) {
        return None;
    }
    Some(build(types, left, right))
}

/// The type a parameter declared as `id` really has, 6.7.6.3p7 and p8.
///
/// An array parameter is a pointer to its element and a function parameter is a pointer to the
/// function, which is why `int f(int a[3])` and `int f(int *a)` declare the same function. The
/// qualifiers on the outermost node go too, so `void f(const int)` and `void f(int)` do as
/// well: a `const` there is a promise the function makes to itself and not part of its type.
///
/// [`FunctionType::params`] is defined to hold types this has already been applied to, so this
/// belongs to whoever builds the type out of a declarator rather than to the comparison below.
pub fn adjust_parameter(types: &mut Types, id: TypeId) -> TypeId {
    let canonical = types.canonical(id);
    match types.kind(canonical) {
        // The element keeps its own qualifiers. The ones C99 allows inside the brackets belong
        // to the pointer that replaces the array, and the parser is what puts them there.
        TypeKind::Array { elem, .. } => types.pointer(elem),
        TypeKind::Function(_) => types.pointer(canonical),
        _ => types.unqualified(id),
    }
}

/// Compatibility, with a stack of record pairs already assumed compatible.
///
/// The stack is what makes a self referential structure terminate. `struct node { struct node
/// *next; }` compared against another declaration of itself comes back to the same pair through
/// the pointer, and the second time it is an assumption rather than a question.
fn same(
    types: &Types,
    left: TypeId,
    right: TypeId,
    assumed: &mut Vec<(RecordId, RecordId)>,
) -> bool {
    let left = types.canonical(left);
    let right = types.canonical(right);
    if left == right {
        return true;
    }
    if types.quals(left) != types.quals(right) {
        // The qualifiers have to match exactly, which is what keeps `const int *` and `int *`
        // apart as parameter types.
        return false;
    }
    match (types.kind(left), types.kind(right)) {
        // Two different enumeration declarations are two different types. Each is compatible
        // with what it is represented in, and whether a redefinition of one tag makes the same
        // type again is a question about enumerator values, which live with the declaration
        // rather than in this table.
        (TypeKind::Enum(_), TypeKind::Enum(_)) => false,
        // An enumeration is compatible with the type it is represented in. Both compilers agree,
        // and it is visible in that a `_Generic` cannot list `enum E` and `unsigned int` both.
        (TypeKind::Enum(id), _) => match types.enum_info(id).underlying {
            Some(underlying) => same(types, underlying, right, assumed),
            None => false,
        },
        (_, TypeKind::Enum(id)) => match types.enum_info(id).underlying {
            Some(underlying) => same(types, left, underlying, assumed),
            None => false,
        },
        (TypeKind::Pointer(a), TypeKind::Pointer(b))
        | (TypeKind::Atomic(a), TypeKind::Atomic(b)) => same(types, a, b, assumed),
        (TypeKind::Array { elem: a, len: x }, TypeKind::Array { elem: b, len: y }) => {
            lengths_agree(x, y) && same(types, a, b, assumed)
        }
        (TypeKind::Vector { elem: a, len: x }, TypeKind::Vector { elem: b, len: y }) => {
            x == y && same(types, a, b, assumed)
        }
        (TypeKind::Function(a), TypeKind::Function(b)) => {
            functions(types, types.signature(a), types.signature(b), assumed)
        }
        (TypeKind::Record(a), TypeKind::Record(b)) => records(types, a, b, assumed),
        _ => false,
    }
}

/// Whether two array lengths are compatible.
///
/// Only two constant sizes can disagree. An array whose size nobody wrote is compatible with
/// any of them, and so is a variable length one, whose size is not known until it runs.
fn lengths_agree(left: ArrayLen, right: ArrayLen) -> bool {
    match (left, right) {
        (ArrayLen::Fixed(a), ArrayLen::Fixed(b)) => a == b,
        _ => true,
    }
}

/// Whether two function types are compatible, 6.7.6.3p15.
fn functions(
    types: &Types,
    left: &FunctionType,
    right: &FunctionType,
    assumed: &mut Vec<(RecordId, RecordId)>,
) -> bool {
    if !same(types, left.ret, right.ret, assumed) {
        return false;
    }
    match (left.prototyped, right.prototyped) {
        (true, true) => {
            left.variadic == right.variadic
                && left.params.len() == right.params.len()
                && left.params.iter().zip(&right.params).all(|(&a, &b)| same(types, a, b, assumed))
        }
        // An old style definition is the one unprototyped type that knows what its parameters
        // are, and 6.7.6.3p15 holds it to a stricter rule than a declaration that knows nothing:
        // the counts have to agree and each prototype parameter has to be compatible with the
        // promoted type of the identifier facing it, which is what the list holds.
        (true, false) if !right.params.is_empty() => defines(types, left, right, assumed),
        (false, true) if !left.params.is_empty() => defines(types, right, left, assumed),
        // An old style declaration says nothing about the parameters, so it is compatible with a
        // prototype only when the call would have gone the same way regardless: no `...`, and no
        // parameter the default argument promotions would have changed on the way in.
        (true, false) => stands_for(types, left),
        (false, true) => stands_for(types, right),
        (false, false) => true,
    }
}

/// Whether a prototype and an old style definition describe the same function, 6.7.6.3p15.
///
/// The rule as written is that the counts agree and each prototype parameter is compatible with
/// the promoted type of the identifier facing it. Taken literally that makes `int f(char);` and a
/// definition of `f` with a `char` identifier two different functions, since `char` promotes to
/// `int`, and every compiler takes that pair because all the code written this way is written
/// against a header. So a prototype parameter the promotions would have changed is allowed to
/// face what it changes into, which is the one relaxation and is what makes the pair work.
fn defines(
    types: &Types,
    proto: &FunctionType,
    def: &FunctionType,
    assumed: &mut Vec<(RecordId, RecordId)>,
) -> bool {
    !proto.variadic
        && proto.params.len() == def.params.len()
        && proto
            .params
            .iter()
            .zip(&def.params)
            .all(|(&a, &b)| same(types, a, b, assumed) || promotes_to(types, a, b))
}

/// Whether the default argument promotions turn the first type into the second.
fn promotes_to(types: &Types, from: TypeId, to: TypeId) -> bool {
    if survives_promotion(types, from) {
        return false;
    }
    let to = types.kind(types.canonical(to));
    match types.kind(types.canonical(from)) {
        TypeKind::Float(FloatKind::Float) => to == TypeKind::Float(FloatKind::Double),
        // Everything else the promotions touch is narrower than an `int` and becomes one. The
        // target where that is not so is one where `int` is no wider than a `short`, which none
        // of the targets here is.
        _ => to == TypeKind::Int(IntKind::Int),
    }
}

/// Whether an old style declaration of a function could stand for this prototype.
fn stands_for(types: &Types, signature: &FunctionType) -> bool {
    !signature.variadic && signature.params.iter().all(|&param| survives_promotion(types, param))
}

/// Whether a parameter type is one the default argument promotions leave alone.
///
/// The `float` case is the one that matters: an old style call passes a `double`, so a prototype
/// taking a `float` is a different function from the same name declared without one, and both
/// compilers refuse the pair.
fn survives_promotion(types: &Types, id: TypeId) -> bool {
    match types.kind(types.canonical(id)) {
        TypeKind::Bool => false,
        TypeKind::Int(kind) => kind.rank() >= IntKind::Int.rank(),
        TypeKind::Float(FloatKind::Float) => false,
        // An enumeration is compatible with what it is represented in, so it comes through
        // whenever that type does.
        TypeKind::Enum(id) => match types.enum_info(id).underlying {
            Some(underlying) => survives_promotion(types, underlying),
            None => false,
        },
        // Everything else, `_BitInt` included, is its own promotion.
        _ => true,
    }
}

/// Whether two record declarations are the same type.
///
/// The same declaration always is. Two different ones are in C23 when they have the same tag and
/// the same members, which is the rule that lets a header be included twice without a guard.
/// clang 18 implements it and gcc 13.3 still rejects the redefinition outright. In the older
/// dialects the question does not arise, because a second definition of a tag in one scope is
/// refused before anything asks whether the two types match.
fn records(
    types: &Types,
    left: RecordId,
    right: RecordId,
    assumed: &mut Vec<(RecordId, RecordId)>,
) -> bool {
    if left == right || assumed.contains(&(left, right)) {
        return true;
    }
    let a = types.record_info(left);
    let b = types.record_info(right);
    if a.kind != b.kind || a.tag.is_none() || a.tag != b.tag {
        // An anonymous record is compatible with nothing but itself. There is no name by which a
        // second declaration could be claiming to be the same type.
        return false;
    }
    if a.layout.is_none() || b.layout.is_none() || a.fields.len() != b.fields.len() {
        // An incomplete declaration has no members to compare. Two mentions of one tag in one
        // scope are one declaration and one id, so they never reach here.
        return false;
    }
    assumed.push((left, right));
    let answer = a
        .fields
        .iter()
        .zip(&b.fields)
        .all(|(x, y)| x.name == y.name && x.bits == y.bits && same(types, x.ty, y.ty, assumed));
    assumed.pop();
    answer
}

/// The composite of two types already known to be compatible.
fn build(types: &mut Types, left: TypeId, right: TypeId) -> TypeId {
    if left == right {
        return left;
    }
    let canonical = types.canonical(left);
    match (types.kind(canonical), types.kind(types.canonical(right))) {
        (TypeKind::Array { elem: a, len: x }, TypeKind::Array { elem: b, len: y }) => {
            let elem = build(types, a, b);
            // The declaration that knew the size is the one to take it from, whichever side it
            // was. An array type carries no qualifiers of its own, they are on the element.
            let len = if matches!(x, ArrayLen::Fixed(_)) { x } else { y };
            types.array(elem, len)
        }
        (TypeKind::Pointer(a), TypeKind::Pointer(b)) => {
            let inner = build(types, a, b);
            let quals = types.quals(canonical);
            let pointer = types.pointer(inner);
            types.qualified(pointer, quals)
        }
        (TypeKind::Function(a), TypeKind::Function(b)) => {
            let a = types.signature(a).clone();
            let b = types.signature(b).clone();
            composite_function(types, &a, &b)
        }
        // Everything else has nothing to combine, and the type as it was written is the better
        // of the two answers because a diagnostic can print the name the program used.
        _ => left,
    }
}

/// The composite of two compatible function types.
fn composite_function(types: &mut Types, left: &FunctionType, right: &FunctionType) -> TypeId {
    let ret = build(types, left.ret, right.ret);
    // The prototype wins, because it is the declaration that knows something. This is what makes
    // `void f(); void f(int);` a function of one `int` afterwards, so that the calls written
    // between the two declarations can still be checked against something.
    let (params, variadic, prototyped) = match (left.prototyped, right.prototyped) {
        (true, true) => {
            let params =
                left.params.iter().zip(&right.params).map(|(&a, &b)| build(types, a, b)).collect();
            (params, left.variadic, true)
        }
        (true, false) => (left.params.clone(), left.variadic, true),
        (false, true) => (right.params.clone(), right.variadic, true),
        // Neither is a prototype, so neither makes a call checkable. What is still worth keeping
        // is an old style definition's parameter list, which is the only thing an unprototyped
        // type ever has one of and which is what its own lowering reads.
        (false, false) => {
            let params =
                if left.params.is_empty() { right.params.clone() } else { left.params.clone() };
            (params, false, false)
        }
    };
    types.function(FunctionType { ret, params, variadic, prototyped })
}
