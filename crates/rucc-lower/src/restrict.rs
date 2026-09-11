//! Which `restrict` scope an access is in, and which pointer inside it the access went through.
//!
//! Design: `spec/optimizer/08-alias-analysis.md` section 8.2 layer 5, and
//! `spec/safe-memory/09-type-init-and-races.md` section 9.6.
//!
//! Two small numbers on every access, a clique and a base, which is the whole of the mechanism and
//! is what gcc calls `MR_DEPENDENCE_CLIQUE` and `MR_DEPENDENCE_BASE`. A clique is one scope that
//! declares `restrict` pointers and a base is one of the pointers it declares. Same clique and
//! different base means the two accesses cannot touch the same byte, because that is exactly what
//! `restrict` promises, and [`rucc_ir::Restrict::disjoint`] is that one line.
//!
//! Layer 5 of the alias analysis has been written and tested since the analysis went in and has
//! never had an access to answer about, because nothing worked out where a pointer came from.
//! This is where it comes from.
//!
//! # What a base is worked out from
//!
//! The names the access was written with, and not the value the pointer turns out to hold. The
//! walk over a place takes the members and the subscripts apart already, so what arrives here is
//! the pointer expression an access is about to be made through, and this follows it down through
//! the casts and the pointer arithmetic to the declaration at the bottom of it. A member of
//! something reached through a `restrict` pointer is reached through it too, which falls out of
//! the place walk rather than being said here. `p->next->value` stops at the inner dereference,
//! because that pointer was read out of memory and where it came from is not a question about
//! names.
//!
//! This is deliberately syntactic. The standard's definition of based on is about what happens to
//! an expression when `P` is changed to point at a copy of the array, which is not a question the
//! front end can answer, and the syntactic reading is what gcc and clang both implement. It is the
//! conservative direction too: an access nothing recognizes carries no clique, and a clique of
//! zero is no information rather than a claim.
//!
//! # Why the clique is numbered per module and not per function
//!
//! Because an inliner is a thing that merges two functions into one, and two functions that each
//! numbered their own parameters clique one would come out of it with four pointers in one clique
//! promising things about each other that nobody promised. gcc renumbers on inlining for exactly
//! this reason. There is no inliner here yet, and numbering from a counter that the whole module
//! shares means there is nothing to renumber when there is one. Merging two modules at link time
//! is the same hazard one level up and is tamnd/rucc#969.
//!
//! # What this does not work out yet
//!
//! A `restrict` pointer declared inside a block rather than as a parameter. The scope it makes is
//! the block, which the walk does not have in hand where the declarations of a function are
//! gathered, and the parameters are where `restrict` is nearly always written: `memcpy`, `strcpy`
//! and the numeric kernels all put it on parameters and all have two. tamnd/rucc#970.

use std::collections::HashMap;

use rucc_ast::{BinaryOp, UnaryOp};
use rucc_ir::Restrict;
use rucc_sema::{Conversion, DeclId, ExprId, ExprKind, Tast};
use rucc_types::{Qualifiers, TypeId, TypeKind, Types, is_pointer};

/// The `restrict` pointers a function declares, and which scope and number each of them has.
#[derive(Debug, Default)]
pub(crate) struct Scopes {
    /// One entry per `restrict` pointer in scope. Empty for the overwhelming majority of
    /// functions, which declare none, and a linear map would do as well at these sizes.
    of: HashMap<DeclId, Restrict>,
}

impl Scopes {
    /// The scope a function's parameter list makes.
    ///
    /// The block a `restrict` parameter's promise covers is the function body, which is what C
    /// 6.7.3.1 says and is why the parameters are one scope rather than one each. A function that
    /// declares none gets no clique at all, so `next` is left alone and nothing in the function
    /// carries a number.
    pub(crate) fn of_params(tast: &Tast, types: &Types, params: &[DeclId], next: &mut u16) -> Self {
        let mut of = HashMap::new();
        let restricted: Vec<DeclId> =
            params.iter().copied().filter(|&decl| qualified(types, tast[decl].ty)).collect();
        if restricted.is_empty() {
            return Self { of };
        }
        // Sixty five thousand scopes in one module is more than any program has, and the answer
        // to running out is to stop saying anything rather than to hand out a number somebody
        // else is already promising things with.
        let Some(clique) = next.checked_add(1) else { return Self { of } };
        *next = clique;
        for (index, decl) in restricted.into_iter().enumerate() {
            let Ok(base) = u16::try_from(index + 1) else { break };
            of.insert(decl, Restrict { clique, base });
        }
        Self { of }
    }

    /// The scope and the pointer an access through this pointer expression goes through.
    ///
    /// Asked about the pointer rather than about the place, because the place is walked by
    /// `crate::body`, which is where the members and the subscripts are taken apart already. What
    /// reaches here is the expression whose value the access is about to be made through.
    pub(crate) fn value(&self, tast: &Tast, types: &Types, expr: ExprId) -> Restrict {
        if self.of.is_empty() {
            return Restrict::NONE;
        }
        match tast[expr].kind {
            ExprKind::Decl(decl) => self.of.get(&decl).copied().unwrap_or(Restrict::NONE),
            // Reading the pointer out of the variable it is in, which is the shape every use of a
            // parameter arrives as. A read of anything else is a pointer that came out of memory
            // and the name it was written with says nothing about where it came from.
            ExprKind::Convert { kind: Conversion::Lvalue, operand } => match tast[operand].kind {
                ExprKind::Decl(decl) => self.of.get(&decl).copied().unwrap_or(Restrict::NONE),
                _ => Restrict::NONE,
            },
            // A cast and the conversions the language performs both leave the pointer pointing
            // where it pointed, so what it was derived from does not change.
            ExprKind::Cast(operand)
            | ExprKind::Convert { operand, .. }
            | ExprKind::Unary { op: UnaryOp::Plus, operand } => self.value(tast, types, operand),
            // `p + n` and `p - n` are derived from `p`, which is the whole reason a loop over a
            // `restrict` parameter is the case worth answering about.
            ExprKind::Binary { op: BinaryOp::Add | BinaryOp::Sub, lhs, rhs } => {
                let one = is_pointer(types, tast[lhs].ty);
                let (pointer, other) = if one { (lhs, rhs) } else { (rhs, lhs) };
                if is_pointer(types, tast[pointer].ty) && !is_pointer(types, tast[other].ty) {
                    self.value(tast, types, pointer)
                } else {
                    // Two pointers, which is a subtraction whose value is not a pointer at all,
                    // or neither, which is not pointer arithmetic.
                    Restrict::NONE
                }
            }
            ExprKind::Comma { rhs, .. } => self.value(tast, types, rhs),
            _ => Restrict::NONE,
        }
    }
}

/// Whether a declaration's type is a pointer somebody wrote `restrict` on.
///
/// The adjusted type, which is what a parameter written `int a[restrict]` has by the time it is
/// here: the adjustment to a pointer carries the qualifiers from inside the brackets, which is
/// where C 6.7.6.3 puts them and is the only reason that spelling means anything.
fn qualified(types: &Types, ty: TypeId) -> bool {
    let canonical = types.canonical(ty);
    matches!(types.kind(canonical), TypeKind::Pointer(_))
        && types.quals(ty).has(Qualifiers::RESTRICT)
}
