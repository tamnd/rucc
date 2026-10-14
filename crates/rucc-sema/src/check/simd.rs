//! `__attribute__((simd))`: a function that can be called from a loop the vectorizer made, with
//! a vector of arguments for each parameter and a vector of results, one lane for each call.
//!
//! gcc 16 builds, beside a function defined with it, the vector versions the x86-64 vector
//! function ABI names, and a loop compiled anywhere else that calls the function from a loop it
//! vectorizes calls those instead, under the same names. This module reads the attribute the way
//! gcc reads it and builds the versions as definitions of their own, measured one case at a time:
//!
//! * No argument asks for a version called with every lane wanted and one called with a mask,
//!   which are `N` and `M` in the name. `"notinbranch"` asks for the first and `"inbranch"` for
//!   the second, every declaration adds what it asks for, and anything else is refused in gcc's
//!   words. On an object the attribute is dropped with gcc's warning.
//! * Only a definition the unit emits with external linkage gets them. gcc gives a `static` one
//!   versions that are thrown away with the function at `-O2` and kept as local symbols at
//!   `-O0`, where nothing outside the unit can call them, and builds none here. A function
//!   `target_clones` builds more than once gets none either, as from gcc.
//! * Every parameter and the return type has to be a scalar of one, two, four or eight bytes:
//!   `_Bool`, an integer, an enumeration, a pointer, `float` or `double`. Anything else, a
//!   structure, a complex number, `long double`, `__int128` or `_Float16`, is reported with gcc's
//!   warning and the function gets no versions. gcc reports the functions last to first, and so
//!   does this.
//! * The lanes are as many as fit in sixteen bytes of what gcc calls the characteristic type,
//!   the return type, or the first parameter's for a function returning `void`, or `int`. A
//!   parameter narrower than that comes in one vector of as many lanes, and a wider one in as
//!   many vectors of sixteen bytes as it takes, `long` beside `char` being eight of two lanes
//!   each. A pointer is a lane of `long`. The mask of an `M` version is one more vector, of
//!   integers as wide as the characteristic type, and a lane is wanted when it is not zero.
//! * The name is `_ZGV`, `b`, `N` or `M`, the number of lanes and a `v` for each parameter, then
//!   an underscore and the function's symbol, an assembler name if it has one: `_ZGVbN2v_sin`.
//!
//! The versions built are gcc's `b` versions, the ones for SSE2, which is every x86-64 processor
//! and what gcc vectorizes for when no `-m` flag or `-march` says otherwise. gcc also builds `c`,
//! `d` and `e` versions, for AVX, AVX2 and AVX-512, which take their vectors in the 32 and 64 byte
//! registers, and the code generator here has no such registers yet, so those are not built: a
//! loop compiled by gcc for one of those extensions that calls the function names a version this
//! unit does not define, which the link reports. Each version calls the function once for each
//! lane, rather than running a vectorized copy of the body as gcc's does, which gives the same
//! answers for the lanes gcc defines. A lane the mask leaves out is zero.
//!
//! On every target but x86-64 ELF the attribute is read and nothing is built, which is what a
//! target with no vector function ABI in gcc does, and `__has_attribute` answers no there.

use rucc_ast::{AttrArg, AttrList};
use rucc_diag::{Diagnostic, Span};
use rucc_types::{FloatKind, IntKind, TypeId, TypeKind};

use crate::check::Checker;
use crate::decl::{DeclFlags, DeclId, DeclKind, Linkage, Startup};
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::stmt::{Stmt, StmtId};
use crate::tast::{Branches, Const, SimdClone};

/// The code of each refusal of what an attribute's arguments say.
const REFUSED: &str = "E0862";

/// The code of the warning about a type the vector versions cannot be built for, which no
/// option controls in gcc either.
const UNSUPPORTED: &str = "E0863";

/// The code of the warning about an `_Atomic` parameter, which gcc gives under `-Wopenmp`.
const ATOMIC: &str = "E0864";

/// How many bits wide each vector of a `b` version is, for integers and floating lanes alike.
const BITS: u32 = 128;

/// One lane of a vector a version passes: its type, and how many bits wide it is.
#[derive(Clone, Copy)]
struct Lane {
    ty: TypeId,
    bits: u32,
}

/// What every version of one function takes and gives back.
struct Shape {
    /// The parameters' types, as the definition declares them.
    params: Vec<TypeId>,
    /// The lane each parameter travels in.
    lanes: Vec<Lane>,
    /// The lane of the return type, nothing for `void`.
    ret: Option<Lane>,
    /// How many lanes each version has, which is how many calls it stands for.
    count: u32,
    /// How many bits wide the characteristic type is, which is how wide a lane of the mask is.
    characteristic: u32,
}

/// Why a type cannot be a lane.
enum Refused {
    /// A type with no lane, such as a structure or `long double`.
    Unsupported,
    /// An `_Atomic` one, which gcc words differently.
    Atomic,
}

impl Checker<'_> {
    /// The calls a `simd` attribute in the lists asks a function to be built for, and nothing
    /// when no list has one or what each one said was refused, which has been reported.
    pub(in crate::check) fn simd_branches(
        &mut self,
        lists: &[AttrList],
        kind: DeclKind,
    ) -> Option<Branches> {
        let ast = self.ast;
        let mut asked: Option<Branches> = None;
        for &list in lists {
            for &attr in &ast[list] {
                if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu")
                    || self.gnu_name(&attr) != "simd"
                {
                    continue;
                }
                let args = &ast[attr.args];
                if args.len() > 1 {
                    let what = "wrong number of arguments specified for 'simd' attribute";
                    let found = format!("expected between 0 and 1, found {}", args.len());
                    let error = Diagnostic::error(what, attr.span).with_code(REFUSED);
                    self.report(error.note(found, attr.span));
                    continue;
                }
                if kind != DeclKind::Function {
                    let what = "'simd' attribute ignored";
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                    continue;
                }
                let branches = match args.first() {
                    None => Branches { notinbranch: true, inbranch: true },
                    Some(&arg) => match self.simd_flag(arg) {
                        Some(Ok(branches)) => branches,
                        Some(Err(())) => {
                            let what = "only 'inbranch' and 'notinbranch' flags are allowed for \
                                        '__simd__' attribute";
                            self.report(Diagnostic::error(what, attr.span).with_code(REFUSED));
                            continue;
                        }
                        None => {
                            let what = "attribute 'simd' argument not a string";
                            self.report(Diagnostic::error(what, attr.span).with_code(REFUSED));
                            continue;
                        }
                    },
                };
                let so_far = asked.get_or_insert_default();
                so_far.notinbranch |= branches.notinbranch;
                so_far.inbranch |= branches.inbranch;
            }
        }
        asked
    }

    /// What one argument of the attribute asks for, an error for a string that is neither
    /// flag, and nothing for an argument that is not a string at all.
    fn simd_flag(&mut self, arg: AttrArg) -> Option<Result<Branches, ()>> {
        let AttrArg::Expr(expr) = arg else { return None };
        let checked = self.expr(expr);
        let ExprKind::Str(id) = self.tast[checked].kind else { return None };
        let text: String =
            self.tast[id].elements.iter().filter_map(|&unit| char::from_u32(unit)).collect();
        Some(match text.as_str() {
            "notinbranch" => Ok(Branches { notinbranch: true, inbranch: false }),
            "inbranch" => Ok(Branches { notinbranch: false, inbranch: true }),
            _ => Err(()),
        })
    }

    /// Builds the vector versions of every function that asked for them and gets them, once the
    /// whole unit has been read, since a declaration below the definition can still ask.
    pub(in crate::check) fn build_simd_clones(&mut self) {
        let tuple = &self.cx.target.tuple;
        if tuple.arch().as_str() != "x86_64" || matches!(tuple.os().as_str(), "windows" | "macos") {
            return;
        }
        let mut asked: Vec<(DeclId, StmtId, Branches)> = self
            .tast
            .simd_asked()
            .filter_map(|(decl, branches)| {
                let node = &self.tast[decl];
                let emitted = node.linkage == Linkage::External && node.inline.emits();
                let built = emitted && self.tast.versions(decl).is_none();
                node.body.filter(|_| built).map(|body| (decl, body, branches))
            })
            .collect();
        // The last definition first, which is the order gcc reports them in.
        asked.sort_by_key(|&(_, body, _)| std::cmp::Reverse(body.index()));
        for (decl, _, branches) in asked {
            self.simd_clones(decl, branches);
        }
    }

    /// The vector versions of one function, or the warning saying why it has none.
    fn simd_clones(&mut self, decl: DeclId, branches: Branches) {
        let at = self.tast.definition_span(decl);
        let node = self.tast[decl].clone();
        let TypeKind::Function(function) = self.types.kind(self.types.canonical(node.ty)) else {
            return;
        };
        let signature = self.types.signature(function).clone();
        let params: Vec<TypeId> =
            self.tast[node.params].iter().map(|&param| self.tast[param].ty).collect();
        let void = matches!(self.types.kind(self.types.canonical(signature.ret)), TypeKind::Void);
        let ret = if void {
            None
        } else {
            match self.simd_lane(signature.ret) {
                Ok(lane) => Some(lane),
                Err(_) => {
                    let what =
                        format!("unsupported return type '{}' for simd", self.spell(signature.ret));
                    self.report(Diagnostic::warning(what, at).with_code(UNSUPPORTED));
                    return;
                }
            }
        };
        let mut lanes = Vec::with_capacity(params.len());
        for &param in &params {
            match self.simd_lane(param) {
                Ok(lane) => lanes.push(lane),
                Err(Refused::Unsupported) => {
                    let what =
                        format!("unsupported argument type '{}' for simd", self.spell(param));
                    self.report(Diagnostic::warning(what, at).with_code(UNSUPPORTED));
                    return;
                }
                Err(Refused::Atomic) => {
                    let what = "ignoring '#pragma omp declare simd' on function with '_Atomic' \
                                qualified non-'uniform' argument";
                    self.report(Diagnostic::warning(what, at).with_code(ATOMIC));
                    return;
                }
            }
        }
        // `int` when there is neither a return value nor a parameter, which is gcc's choice.
        let characteristic = ret.or(lanes.first().copied()).map_or(32, |lane| lane.bits);
        let shape = Shape { params, lanes, ret, count: BITS / characteristic, characteristic };
        let mut clones = Vec::new();
        for (masked, wanted) in [(false, branches.notinbranch), (true, branches.inbranch)] {
            if !wanted {
                continue;
            }
            let clone = self.simd_clone(decl, &shape, masked, at);
            let letter = if masked { 'M' } else { 'N' };
            let mut prefix = format!("_ZGVb{letter}{}", shape.count);
            prefix.extend(shape.params.iter().map(|_| 'v'));
            clones.push(SimdClone { decl: clone, prefix });
        }
        self.tast.record_simd_clones(decl, clones);
    }

    /// One vector version of `decl`: a definition taking each parameter's lanes in vectors, and
    /// a mask after them when `masked`, that calls `decl` once for each lane and gives back what
    /// each call returned in a vector with a lane for each.
    fn simd_clone(&mut self, decl: DeclId, shape: &Shape, masked: bool, at: Span) -> DeclId {
        let Shape { ref params, ref lanes, ret, count, characteristic } = *shape;
        let node = self.tast[decl].clone();
        let TypeKind::Function(function) = self.types.kind(self.types.canonical(node.ty)) else {
            unreachable!("a function's type is a function type");
        };
        let mut signature = self.types.signature(function).clone();
        // Each parameter's vectors, as many as it takes to hold `count` lanes.
        let mut vectors: Vec<(DeclId, u32)> = Vec::new();
        let mut takes: Vec<TypeId> = Vec::new();
        let mut first = Vec::with_capacity(lanes.len());
        for lane in lanes {
            let width = count.min(BITS / lane.bits);
            let ty = self.types.vector(lane.ty, width);
            first.push(vectors.len());
            for _ in 0..count / width {
                vectors.push((self.unnamed_object(ty, at), width));
                takes.push(ty);
            }
        }
        let mask = masked.then(|| {
            let ty = self.types.vector(self.types.int(int_of_bits(characteristic, true)), count);
            takes.push(ty);
            self.unnamed_object(ty, at)
        });
        let result = ret.map(|lane| self.types.vector(lane.ty, count));
        signature.ret = result.unwrap_or(signature.ret);
        signature.params = takes;
        signature.variadic = false;
        signature.prototyped = true;
        let ty = self.types.function(signature.clone());
        let mut declared: Vec<DeclId> = vectors.iter().map(|&(param, _)| param).collect();
        declared.extend(mask);
        let mut body = Vec::new();
        let results = result.map(|vector| self.unnamed_object(vector, at));
        if let Some(results) = results {
            let decls = self.tast.add_decl_refs(&[results]);
            body.push(self.tast.stmt(Stmt::Decls(decls), at));
        }
        let original = self.types.signature(function).clone();
        for index in 0..count {
            let mut args = Vec::with_capacity(lanes.len());
            for (which, lane) in lanes.iter().enumerate() {
                let width = count.min(BITS / lane.bits);
                let (vector, _) = vectors[first[which] + (index / width) as usize];
                let read = self.simd_lane_of(vector, index % width, at);
                let arg = self.simd_cast(read, params[which], at);
                let arg = match original.params.get(which) {
                    Some(&param) if original.prototyped => self.conv().to_type(arg, param),
                    _ => self.default_promote(arg),
                };
                args.push(arg);
            }
            let callee = Expr::new(ExprKind::Decl(decl), node.ty, Category::Function);
            let callee = self.tast.expr(callee, at);
            let callee = self.value(callee);
            let args = self.tast.add_expr_refs(&args);
            let returns = self.types.unqualified(original.ret);
            let call = Expr::new(ExprKind::Call { callee, args }, returns, Category::Rvalue);
            let call = self.tast.expr(call, at);
            let stmt = match (results, ret) {
                (Some(results), Some(lane)) => {
                    let value = self.simd_cast(call, lane.ty, at);
                    self.simd_store(results, index, value, at)
                }
                _ => self.tast.stmt(Stmt::Expr(call), at),
            };
            let stmt = match mask {
                Some(mask) => {
                    let wanted = self.simd_lane_of(mask, index, at);
                    let cond = self.condition(wanted, at);
                    let otherwise = match (results, ret) {
                        (Some(results), Some(lane)) => {
                            let zero = self.constant(Const::Int(0), self.int(), at);
                            let zero = self.simd_cast(zero, lane.ty, at);
                            Some(self.simd_store(results, index, zero, at))
                        }
                        _ => None,
                    };
                    self.tast.stmt(Stmt::If { cond, then: stmt, otherwise }, at)
                }
                None => stmt,
            };
            body.push(stmt);
        }
        if let (Some(results), Some(vector)) = (results, result) {
            let read = Expr::new(ExprKind::Decl(results), vector, Category::Lvalue);
            let read = self.tast.expr(read, at);
            let read = self.value(read);
            body.push(self.tast.stmt(Stmt::Return(Some(read)), at));
        }
        let list = self.tast.add_stmt_refs(&body);
        let block = self.tast.stmt(Stmt::Block(list), at);
        let mut clone = node;
        clone.ty = ty;
        clone.params = self.tast.add_decl_refs(&declared);
        clone.body = Some(block);
        clone.asm_label = None;
        clone.alias = None;
        clone.init = None;
        clone.cleanup = None;
        clone.alignment = None;
        clone.flags = DeclFlags::NONE;
        clone.startup = Startup::default();
        self.tast.decl(clone, at)
    }

    /// The lane a function's parameter or return type is passed in, or why it has none.
    fn simd_lane(&self, ty: TypeId) -> Result<Lane, Refused> {
        let canonical = self.types.canonical(ty);
        let int = |kind| Ok(Lane { ty: self.types.int(kind), bits: int_bits(kind) });
        match self.types.kind(canonical) {
            TypeKind::Bool => int(IntKind::UChar),
            TypeKind::Int(IntKind::Int128 | IntKind::UInt128) => Err(Refused::Unsupported),
            TypeKind::Int(kind) => int(kind),
            TypeKind::Pointer(_) => int(IntKind::Long),
            TypeKind::Float(FloatKind::Float | FloatKind::Float32) => {
                Ok(Lane { ty: self.types.float(FloatKind::Float), bits: 32 })
            }
            TypeKind::Float(FloatKind::Double | FloatKind::Float64 | FloatKind::Float32x) => {
                Ok(Lane { ty: self.types.float(FloatKind::Double), bits: 64 })
            }
            TypeKind::Enum(id) => match self.types.enum_info(id).underlying {
                Some(underlying) => self.simd_lane(underlying),
                None => Err(Refused::Unsupported),
            },
            // As wide as the smallest integer it fits in, which is the mode gcc gives it.
            TypeKind::BitInt { signed, width } if width <= 64 => {
                let bits = width.next_power_of_two().max(8);
                int(int_of_bits(bits, signed))
            }
            TypeKind::Atomic(_) => Err(Refused::Atomic),
            _ => Err(Refused::Unsupported),
        }
    }

    /// Lane `index` of a vector parameter or local, read.
    fn simd_lane_of(&mut self, vector: DeclId, index: u32, at: Span) -> ExprId {
        let ty = self.tast[vector].ty;
        let base = self.tast.expr(Expr::new(ExprKind::Decl(vector), ty, Category::Lvalue), at);
        let index = self.constant(Const::Int(i128::from(index)), self.int(), at);
        let lane = rucc_types::element(&self.types, ty).expect("a vector");
        let lane = Expr::new(ExprKind::Subscript { base, index }, lane, Category::Lvalue);
        let lane = self.tast.expr(lane, at);
        self.value(lane)
    }

    /// `results[index] = value;`.
    fn simd_store(&mut self, results: DeclId, index: u32, value: ExprId, at: Span) -> StmtId {
        let ty = self.tast[results].ty;
        let base = self.tast.expr(Expr::new(ExprKind::Decl(results), ty, Category::Lvalue), at);
        let place = self.constant(Const::Int(i128::from(index)), self.int(), at);
        let lane = rucc_types::element(&self.types, ty).expect("a vector");
        let place = Expr::new(ExprKind::Subscript { base, index: place }, lane, Category::Lvalue);
        let lhs = self.tast.expr(place, at);
        let node = ExprKind::Assign { op: None, computation: lane, lhs, rhs: value };
        let assign = self.tast.expr(Expr::new(node, lane, Category::Rvalue), at);
        self.tast.stmt(Stmt::Expr(assign), at)
    }

    /// A value made the type `to` the way a cast the program wrote would make it, which is what
    /// takes a lane of `long` to a pointer and back.
    fn simd_cast(&mut self, value: ExprId, to: TypeId, at: Span) -> ExprId {
        let to = self.types.unqualified(to);
        let from = self.tast[value].ty;
        if self.types.canonical(from) == self.types.canonical(to) {
            return value;
        }
        self.tast.expr(Expr::new(ExprKind::Cast(value), to, Category::Rvalue), at)
    }
}

/// How many bits an integer of the kind is on x86-64.
fn int_bits(kind: IntKind) -> u32 {
    match kind {
        IntKind::Char | IntKind::SChar | IntKind::UChar => 8,
        IntKind::Short | IntKind::UShort => 16,
        IntKind::Int | IntKind::UInt => 32,
        IntKind::Long | IntKind::ULong | IntKind::LongLong | IntKind::ULongLong => 64,
        IntKind::Int128 | IntKind::UInt128 => 128,
    }
}

/// The integer kind of `bits` bits on x86-64, signed or not.
fn int_of_bits(bits: u32, signed: bool) -> IntKind {
    match (bits, signed) {
        (8, true) => IntKind::SChar,
        (8, false) => IntKind::UChar,
        (16, true) => IntKind::Short,
        (16, false) => IntKind::UShort,
        (32, true) => IntKind::Int,
        (32, false) => IntKind::UInt,
        (_, true) => IntKind::Long,
        (_, false) => IntKind::ULong,
    }
}
