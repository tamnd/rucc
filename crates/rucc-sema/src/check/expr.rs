//! Checking an expression, which is where the constraints of 6.5 live.
//!
//! Design: `spec/07-types-and-semantics.md` sections 7.2 and 7.4.
//!
//! Every function here takes a node of the untyped tree and gives back a node of the typed one,
//! and the shape of each is the same three steps in the same order. Check the operands, decide
//! whether the types the operands turned out to have are ones the operator accepts, and write
//! the conversions the operator performs before writing the operator itself. The conversions are
//! [`Conv`](crate::Conv) and nothing here writes one by hand, which is what keeps the rules in
//! one place rather than in every operator that happens to need them.
//!
//! # The diagnostics
//!
//! The wording is gcc's, taken from gcc 13.3 rather than from memory, because the message is
//! what a person building a real project actually sees and a build script that greps for
//! `incompatible pointer type` is a real thing. What is deliberately not copied is the
//! `[-Wsomething]` suffix gcc prints, since that is the renderer naming the option that
//! controls a warning and this compiler has no such option to name yet.
//!
//! # What is not here
//!
//! The operators that name a type are in `check/expr/typeop.rs`, which is a module of its own
//! because they have nothing in common with the ones here except being expressions: each of
//! them asks the type builder a question and most of them answer with a constant rather than
//! with a computation.
//!
//! A compound literal is in `check/init.rs`, because what it is is an object with an
//! initializer, and everything about it except being an expression belongs to initialization.

use rucc_ast::{self as ast, BinaryOp, UnaryOp};
use rucc_base::Symbol;
use rucc_base::float::Format;
use rucc_diag::{Diagnostic, Span};
use rucc_lex::{Encoding, FloatConstantType, IntConstantType};
use rucc_types::{
    ArrayLen, FloatKind, IntKind, Qualifiers, RecordId, RecordKind, TypeId, TypeKind, compatible,
    is_arithmetic, is_array, is_complete, is_function, is_integer, is_pointer, is_record,
    is_scalar, is_void, pointee,
};

use crate::check::Checker;
use crate::check::expr::typeop::Measure;
use crate::decl::DeclKind;
use crate::eval;
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::scope::Binding;
use crate::tast::Const;

mod typeop;

/// Where a value is being put, for the diagnostics that say so.
///
/// The two read differently in gcc and the difference is not cosmetic: an assignment names both
/// types and an argument names the function and the position, because that is the part a person
/// reading the message needs in each case.
#[derive(Debug, Clone, Copy)]
pub(in crate::check) enum Target {
    /// The right side of an assignment.
    Assignment,
    /// An argument of a call.
    Argument {
        /// Which argument, counting from one the way the message prints it.
        index: usize,
        /// The function, when the call named one rather than computing it.
        function: Option<Symbol>,
    },
    /// The initializer of a declaration.
    Initialization,
    /// The value of a `return`, whose messages name the source type first, since the target is
    /// the function's and is written somewhere else.
    Return,
}

impl Checker<'_> {
    /// Checks one expression and gives back the node it became.
    pub(crate) fn expr(&mut self, id: ast::ExprId) -> ExprId {
        let span = self.ast.expr_span(id);
        match self.ast[id] {
            ast::Expr::Error => self.poison(span),
            ast::Expr::Name(name) => self.name(name, span),
            ast::Expr::Int(value) => self.int_constant(value, span),
            ast::Expr::Float(value) => self.float_constant(value, span),
            ast::Expr::Char(value) => self.char_constant(value, span),
            ast::Expr::Str(value) => self.string(value, span),
            ast::Expr::Bool(value) => self.bool_constant(value, span),
            ast::Expr::Nullptr => self.nullptr(span),
            ast::Expr::Index { base, index } => self.subscript(base, index, span),
            ast::Expr::Call { callee, args } => self.call(callee, args, span),
            ast::Expr::Member { base, name, arrow } => self.member(base, name, arrow, span),
            ast::Expr::Unary { op, operand } => self.unary(op, operand, span),
            ast::Expr::Binary { op, lhs, rhs } => self.binary(op, lhs, rhs, span),
            ast::Expr::Assign { op, lhs, rhs } => self.assign(op, lhs, rhs, span),
            ast::Expr::Cond { cond, then, otherwise } => {
                self.conditional(cond, then, otherwise, span)
            }
            ast::Expr::Comma { lhs, rhs } => self.comma(lhs, rhs, span),
            // `__extension__` turns the pedantic warnings off inside itself, which is a thing
            // to do to the diagnostics and not to the value, so the node it produces is its
            // operand's. Suppressing the warnings waits on `-pedantic` being wired through.
            ast::Expr::Extension(operand) => self.expr(operand),
            ast::Expr::Cast { ty, operand } => self.cast(ty, operand, span),
            ast::Expr::SizeofExpr(operand) => self.measure_expr(operand, Measure::Size, span),
            ast::Expr::SizeofType(ty) => self.measure_type(ty, Measure::Size, span),
            ast::Expr::AlignofExpr(operand) => self.measure_expr(operand, Measure::Align, span),
            ast::Expr::AlignofType(ty) => self.measure_type(ty, Measure::Align, span),
            ast::Expr::Generic { control, assocs } => self.generic(control, assocs, span),
            ast::Expr::Offsetof { ty, path } => self.offset_of(ty, path, span),
            ast::Expr::ChooseExpr { cond, then, otherwise } => {
                self.choose_expr(cond, then, otherwise, span)
            }
            ast::Expr::TypesCompatible { a, b } => self.types_compatible(a, b, span),
            ast::Expr::VaArg { list, ty } => self.va_arg(list, ty, span),
            ast::Expr::CompoundLiteral { ty, init } => self.compound_literal(ty, init, span),
            ast::Expr::StmtExpr(body) => self.stmt_expr(body, span),
            ast::Expr::LabelAddr(name) => self.label_addr(name, span),
        }
    }

    /// An identifier, resolved against the scopes.
    fn name(&mut self, name: Symbol, span: Span) -> ExprId {
        match self.scopes.lookup(name) {
            Some(Binding::Decl(decl)) => {
                // C23 puts a name in scope at the end of its declarator, so `int x = sizeof(x);`
                // is a legal thing to write and this is one of the few places it matters. A
                // declaration that has no type or no value until its initializer is checked is a
                // different matter: there is nothing here yet to take the size of.
                if self.underspecified.contains(&decl) {
                    let spelled = self.text(name).to_owned();
                    self.report(
                        Diagnostic::error(
                            format!("underspecified '{spelled}' referenced in its initializer"),
                            span,
                        )
                        .with_code("E0650"),
                    );
                    return self.poison(span);
                }
                let ty = self.tast[decl].ty;
                let category = match self.tast[decl].kind {
                    DeclKind::Function => Category::Function,
                    DeclKind::Object => Category::Lvalue,
                };
                self.tast.expr(Expr::new(ExprKind::Decl(decl), ty, category), span)
            }
            // An enumerator is a constant. The declaration it came from is not in the tree and
            // does not need to be: what the program can do with it is exactly what it can do
            // with the number, and every reader of the tree would otherwise have to look it up.
            Some(Binding::Enumerator { value, ty }) => self.constant(Const::Int(value), ty, span),
            Some(Binding::Typedef(_)) => {
                let name = self.text(name).to_owned();
                self.report(Diagnostic::error(
                    format!("'{name}' is a type name, not an expression"),
                    span,
                ));
                self.poison(span)
            }
            None => {
                // Once per function, which is what the wording promises. gcc says as much in a
                // note under the first one, and a file with a misspelled name used in a loop is
                // otherwise a screen of the same sentence.
                if self.first_undeclared_use(name) {
                    let spelled = self.text(name).to_owned();
                    self.report(
                        Diagnostic::error(
                            format!("'{spelled}' undeclared (first use in this function)"),
                            span,
                        )
                        .with_code("E0500"),
                    );
                }
                self.poison(span)
            }
        }
    }

    /// An integer constant, which the lexer has already given a type.
    fn int_constant(&mut self, id: ast::IntId, span: Span) -> ExprId {
        let ast = self.ast;
        let constant = &ast[id];
        let ty = match constant.ty {
            IntConstantType::Standard(kind) => self.types.int(kind),
            IntConstantType::BitInt { signed, width } => self.types.bit_int(signed, width),
        };
        // The value is held in a hundred and twenty eight bits whatever its type, and the cast
        // is a reinterpretation and not a conversion: the lexer never produces a value the type
        // it chose cannot hold, so nothing is lost either way.
        let value = constant.value as i128;
        self.constant(Const::Int(value), ty, span)
    }

    /// A floating constant.
    fn float_constant(&mut self, id: ast::FloatId, span: Span) -> ExprId {
        let ast = self.ast;
        let constant = &ast[id];
        if constant.imaginary {
            return self.unsupported("an imaginary constant", span);
        }
        // One suffix, one type. `0.1f32` is a `_Float32` and not a `float`, even though the two
        // are the same format, because they are two types and `_Generic` can tell them apart.
        let kind = match constant.ty {
            FloatConstantType::Float => FloatKind::Float,
            FloatConstantType::Double => FloatKind::Double,
            FloatConstantType::LongDouble => FloatKind::LongDouble,
            FloatConstantType::Float16 => FloatKind::Float16,
            FloatConstantType::Float32 => FloatKind::Float32,
            FloatConstantType::Float64 => FloatKind::Float64,
            FloatConstantType::Float128 => FloatKind::Float128,
            FloatConstantType::Float32x => FloatKind::Float32x,
            FloatConstantType::Float64x => FloatKind::Float64x,
            // `__float80` is the x87 type and not a second type beside it, and the lexer has
            // already turned the suffix away on a target with no x87 type. So it is `long
            // double` where that is the x87 format, and `_Float64x`, which is that format on
            // x86 whatever the operating system has done to `long double`, where it is not.
            FloatConstantType::Float80 => {
                if self.cx.target.long_double_format == Format::X87Extended {
                    FloatKind::LongDouble
                } else {
                    FloatKind::Float64x
                }
            }
        };
        let value = constant.value;
        let ty = self.types.float(kind);
        self.constant(Const::Float(value), ty, span)
    }

    /// A character constant, which in C is an `int` unless it was written with a prefix.
    fn char_constant(&mut self, id: ast::CharId, span: Span) -> ExprId {
        let ast = self.ast;
        let constant = &ast[id];
        let ty = match constant.encoding {
            // `'a'` has type `int` in C and `char` in C++, and the difference is visible: it is
            // why `sizeof 'a'` is four here and one there.
            Encoding::Plain => self.int(),
            Encoding::Utf8 => self.types.int(IntKind::UChar),
            Encoding::Utf16 => self.types.int(IntKind::UShort),
            Encoding::Utf32 => self.types.int(IntKind::UInt),
            Encoding::Wide => self.wide_char(),
        };
        let value = i128::from(constant.value);
        self.constant(Const::Int(value), ty, span)
    }

    /// A string literal, which is an array of characters and an lvalue.
    fn string(&mut self, id: ast::StrId, span: Span) -> ExprId {
        let ast = self.ast;
        let literal = ast[id].clone();
        let elem = match literal.encoding {
            Encoding::Plain => self.types.int(IntKind::Char),
            Encoding::Utf8 => self.types.int(IntKind::UChar),
            Encoding::Utf16 => self.types.int(IntKind::UShort),
            Encoding::Utf32 => self.types.int(IntKind::UInt),
            Encoding::Wide => self.wide_char(),
        };
        // The terminator is part of the type and not part of the spelling, which is why the
        // array is one longer than the literal has elements.
        let len = literal.elements.len() as u64 + 1;
        let ty = self.types.array(elem, ArrayLen::Fixed(len));
        let literal = self.tast.add_string(literal);
        self.tast.expr(Expr::new(ExprKind::Str(literal), ty, Category::Lvalue), span)
    }

    /// `true` or `false`, which C23 made constants of type `bool`.
    fn bool_constant(&mut self, value: bool, span: Span) -> ExprId {
        let ty = self.types.boolean();
        self.constant(Const::Int(i128::from(value)), ty, span)
    }

    /// `nullptr`.
    ///
    /// C23 gives it the type `nullptr_t`, which is a type of its own with one value. There is no
    /// such type here yet, so it is a null pointer constant of type `void *`, which is what it
    /// converts to everywhere a program can currently observe. The difference shows up in
    /// `_Generic` and in `sizeof`, both of which are unsupported above, so nothing can see it
    /// yet, and it is recorded here so that whoever adds the type knows where to look.
    fn nullptr(&mut self, span: Span) -> ExprId {
        let void = self.types.void();
        let ty = self.types.pointer(void);
        self.constant(Const::Int(0), ty, span)
    }

    /// `base[index]`, where either operand may be the pointer.
    fn subscript(&mut self, base: ast::ExprId, index: ast::ExprId, span: Span) -> ExprId {
        let base = self.expr(base);
        let index = self.expr(index);
        let base = self.value(base);
        let index = self.value(index);
        if self.is_poisoned(base) || self.is_poisoned(index) {
            return self.poison(span);
        }
        // `a[i]` and `i[a]` are the same expression, and the tree keeps the pointer first
        // however it was written so that nothing downstream has to ask again.
        let (base, index) = if is_pointer(&self.types, self.tast[base].ty) {
            (base, index)
        } else if is_pointer(&self.types, self.tast[index].ty) {
            (index, base)
        } else {
            self.report(
                Diagnostic::error(
                    "subscripted value is neither array nor pointer nor vector",
                    span,
                )
                .with_code("E0504"),
            );
            return self.poison(span);
        };
        if !is_integer(&self.types, self.tast[index].ty) {
            self.report(
                Diagnostic::error("array subscript is not an integer", span).with_code("E0504"),
            );
            return self.poison(span);
        }
        let index = self.conv().promote(index);
        let elem = pointee(&self.types, self.tast[base].ty).expect("a pointer");
        if is_function(&self.types, elem) {
            self.report(
                Diagnostic::error("subscripted value is pointer to function", span)
                    .with_code("E0504"),
            );
            return self.poison(span);
        }
        if !self.target_of_indirection(elem, span) {
            return self.poison(span);
        }
        self.tast.expr(Expr::new(ExprKind::Subscript { base, index }, elem, Category::Lvalue), span)
    }

    /// `callee(args)`.
    fn call(&mut self, callee: ast::ExprId, args: ast::ExprList, span: Span) -> ExprId {
        // The name is taken from the source and not from the declaration the callee resolved
        // to, because a call through a pointer has no declaration to take it from and gcc names
        // what was written either way.
        let function = match self.ast[callee] {
            ast::Expr::Name(name) => Some(name),
            _ => None,
        };
        let callee = self.expr(callee);
        let callee = self.value(callee);
        let written: Vec<ast::ExprId> = self.ast[args].to_vec();
        // Each argument is a value before it is anything else. An array argument has to have
        // decayed before its type is compared with the parameter's, or passing `char[8]` where
        // `const char *` is wanted is a type error rather than the everyday thing it is.
        let checked: Vec<ExprId> = written
            .into_iter()
            .map(|arg| {
                let arg = self.expr(arg);
                self.value(arg)
            })
            .collect();

        let signature = pointee(&self.types, self.tast[callee].ty)
            .map(|target| self.types.canonical(target))
            .and_then(|target| match self.types.kind(target) {
                TypeKind::Function(id) => Some(self.types.signature(id).clone()),
                _ => None,
            });
        let Some(signature) = signature else {
            if !self.is_poisoned(callee) {
                let what = match function {
                    Some(name) => format!("called object '{}' is not a function", self.text(name)),
                    None => "called object is not a function".to_owned(),
                };
                self.report(
                    Diagnostic::error(format!("{what} or function pointer"), span)
                        .with_code("E0501"),
                );
            }
            return self.poison(span);
        };

        if signature.prototyped {
            let (wanted, given) = (signature.params.len(), checked.len());
            let quoted = function.map(|name| format!(" '{}'", self.text(name))).unwrap_or_default();
            if given < wanted {
                self.report(
                    Diagnostic::error(format!("too few arguments to function{quoted}"), span)
                        .with_code("E0511"),
                );
            } else if given > wanted && !signature.variadic {
                self.report(
                    Diagnostic::error(format!("too many arguments to function{quoted}"), span)
                        .with_code("E0511"),
                );
            }
        }

        let mut args = Vec::with_capacity(checked.len());
        for (index, arg) in checked.into_iter().enumerate() {
            let at = self.tast.expr_span(arg);
            // A parameter the prototype names converts; an argument beyond the prototype, or one
            // to a function declared without a prototype at all, takes the default argument
            // promotions instead, which is what makes `printf("%d", 'c')` pass an `int`.
            let arg = match signature.params.get(index) {
                Some(&param) if signature.prototyped => {
                    let to = Target::Argument { index: index + 1, function };
                    self.assign_to(param, arg, at, to)
                }
                _ => self.default_promote(arg),
            };
            args.push(arg);
        }
        let args = self.tast.add_expr_refs(&args);
        let ty = signature.ret;
        self.tast.expr(Expr::new(ExprKind::Call { callee, args }, ty, Category::Rvalue), span)
    }

    /// `base.name`, and `base->name`, which becomes a dereference and then a member.
    fn member(&mut self, base: ast::ExprId, name: Symbol, arrow: bool, span: Span) -> ExprId {
        let mut base = self.expr(base);
        if arrow {
            base = self.value(base);
            if self.is_poisoned(base) {
                return self.poison(span);
            }
            let ty = self.tast[base].ty;
            let Some(target) = pointee(&self.types, ty) else {
                let ty = self.spell(ty);
                self.report(
                    Diagnostic::error(format!("invalid type argument of '->' (have '{ty}')"), span)
                        .with_code("E0502"),
                );
                return self.poison(span);
            };
            if !self.target_of_indirection(target, span) {
                return self.poison(span);
            }
            let node = ExprKind::Unary { op: UnaryOp::Deref, operand: base };
            base = self.tast.expr(Expr::new(node, target, Category::Lvalue), span);
        }
        if self.is_poisoned(base) {
            return self.poison(span);
        }
        let ty = self.tast[base].ty;
        let TypeKind::Record(record) = self.types.kind(self.types.canonical(ty)) else {
            let name = self.text(name).to_owned();
            self.report(
                Diagnostic::error(
                    format!("request for member '{name}' in something not a structure or union"),
                    span,
                )
                .with_code("E0502"),
            );
            return self.poison(span);
        };
        if !is_complete(&self.types, ty) {
            let ty = self.spell(ty);
            self.report(
                Diagnostic::error(format!("invalid use of undefined type '{ty}'"), span)
                    .with_code("E0503"),
            );
            return self.poison(span);
        }
        let Some(path) = self.find_field(record, name) else {
            let (ty, name) = (self.spell(ty), self.text(name).to_owned());
            self.report(
                Diagnostic::error(format!("'{ty}' has no member named '{name}'"), span)
                    .with_code("E0502"),
            );
            return self.poison(span);
        };
        // An anonymous member is reached through the member that holds it, so `u.x` where `x`
        // is inside an anonymous union is two nodes and not one. Writing the chain out here is
        // what lets everything downstream treat a member access as one step.
        for index in path {
            base = self.member_node(base, index, span);
        }
        base
    }

    /// One step of a member access, whose base has already been checked to be a record.
    fn member_node(&mut self, base: ExprId, index: u32, span: Span) -> ExprId {
        let base_ty = self.tast[base].ty;
        let TypeKind::Record(record) = self.types.kind(self.types.canonical(base_ty)) else {
            unreachable!("the base of a member access is a record");
        };
        let field = self.types.record_info(record).fields[index as usize];
        // The qualifiers of the object reach its members: a member of a `const struct` is
        // `const` whatever the member was declared as, which is what stops `s.x = 1` on one.
        let quals = self.types.quals(base_ty);
        let ty = if quals.is_none() {
            field.ty
        } else {
            let merged = self.types.quals(field.ty).with(quals);
            self.types.qualified(field.ty, merged)
        };
        let category = match (field.is_bit_field(), self.tast[base].category) {
            (true, _) => Category::Bitfield,
            (false, Category::Rvalue) => Category::Rvalue,
            (false, _) => Category::Lvalue,
        };
        let node = ExprKind::Member { base, field: index };
        self.tast.expr(Expr::new(node, ty, category), span)
    }

    /// Where a member lives, as the chain of indices that reaches it.
    ///
    /// A named member of the record itself wins over one reached through an anonymous member,
    /// which is why this looks twice rather than once: a single walk in declaration order would
    /// let an anonymous member's `x` hide the record's own.
    pub(in crate::check) fn find_field(&self, record: RecordId, name: Symbol) -> Option<Vec<u32>> {
        let fields = &self.types.record_info(record).fields;
        for (index, field) in fields.iter().enumerate() {
            if field.name == Some(name) {
                return Some(vec![index as u32]);
            }
        }
        for (index, field) in fields.iter().enumerate() {
            if field.name.is_some() {
                continue;
            }
            let TypeKind::Record(inner) = self.types.kind(self.types.canonical(field.ty)) else {
                continue;
            };
            if let Some(mut path) = self.find_field(inner, name) {
                path.insert(0, index as u32);
                return Some(path);
            }
        }
        None
    }

    /// A prefix or postfix operator on one operand.
    fn unary(&mut self, op: UnaryOp, operand: ast::ExprId, span: Span) -> ExprId {
        let operand = self.expr(operand);
        match op {
            UnaryOp::AddrOf => self.address_of(operand, span),
            UnaryOp::PreInc | UnaryOp::PreDec | UnaryOp::PostInc | UnaryOp::PostDec => {
                self.increment(op, operand, span)
            }
            UnaryOp::Deref => self.dereference(operand, span),
            UnaryOp::Plus | UnaryOp::Minus => {
                let operand = self.conv().promote(operand);
                if self.is_poisoned(operand) {
                    return self.poison(span);
                }
                if !is_arithmetic(&self.types, self.tast[operand].ty) {
                    let what = if op == UnaryOp::Plus { "plus" } else { "minus" };
                    return self.wrong_operand(&format!("unary {what}"), span);
                }
                let ty = self.tast[operand].ty;
                self.tast
                    .expr(Expr::new(ExprKind::Unary { op, operand }, ty, Category::Rvalue), span)
            }
            UnaryOp::BitNot => {
                let operand = self.conv().promote(operand);
                if self.is_poisoned(operand) {
                    return self.poison(span);
                }
                if !is_integer(&self.types, self.tast[operand].ty) {
                    return self.wrong_operand("bit-complement", span);
                }
                let ty = self.tast[operand].ty;
                self.tast
                    .expr(Expr::new(ExprKind::Unary { op, operand }, ty, Category::Rvalue), span)
            }
            UnaryOp::Not => {
                let operand = self.value(operand);
                if self.is_poisoned(operand) {
                    return self.poison(span);
                }
                if !is_scalar(&self.types, self.tast[operand].ty) {
                    return self.wrong_operand("unary exclamation mark", span);
                }
                let operand = self.conv().to_bool(operand);
                let ty = self.int();
                self.tast
                    .expr(Expr::new(ExprKind::Unary { op, operand }, ty, Category::Rvalue), span)
            }
            UnaryOp::Real | UnaryOp::Imag => {
                let operand = self.value(operand);
                if self.is_poisoned(operand) {
                    return self.poison(span);
                }
                let ty = self.tast[operand].ty;
                let what = if op == UnaryOp::Real { "__real__" } else { "__imag__" };
                let ty = match self.types.kind(self.types.canonical(ty)) {
                    TypeKind::Complex(kind) => self.types.float(kind),
                    _ if is_arithmetic(&self.types, ty) => ty,
                    _ => return self.wrong_operand(what, span),
                };
                self.tast
                    .expr(Expr::new(ExprKind::Unary { op, operand }, ty, Category::Rvalue), span)
            }
        }
    }

    /// `*operand`.
    fn dereference(&mut self, operand: ExprId, span: Span) -> ExprId {
        let operand = self.value(operand);
        if self.is_poisoned(operand) {
            return self.poison(span);
        }
        let ty = self.tast[operand].ty;
        let Some(target) = pointee(&self.types, ty) else {
            let ty = self.spell(ty);
            self.report(
                Diagnostic::error(
                    format!("invalid type argument of unary '*' (have '{ty}')"),
                    span,
                )
                .with_code("E0505"),
            );
            return self.poison(span);
        };
        if !self.target_of_indirection(target, span) {
            return self.poison(span);
        }
        // Dereferencing a function pointer gives the function back, which is why `(*f)()` and
        // `f()` are the same call and why this is not an lvalue.
        let category =
            if is_function(&self.types, target) { Category::Function } else { Category::Lvalue };
        let node = ExprKind::Unary { op: UnaryOp::Deref, operand };
        self.tast.expr(Expr::new(node, target, category), span)
    }

    /// `&operand`.
    fn address_of(&mut self, operand: ExprId, span: Span) -> ExprId {
        if self.is_poisoned(operand) {
            return self.poison(span);
        }
        // No `value` call anywhere here, which is the whole point: `&a` on an array is a
        // pointer to the array and not a pointer to its first element, and `&f` is a pointer to
        // the function rather than a pointer to a pointer to it.
        match self.tast[operand].category {
            Category::Lvalue | Category::Function => {}
            Category::Bitfield => {
                let what = self.field_name(operand);
                self.report(
                    Diagnostic::error(format!("cannot take address of bit-field {what}"), span)
                        .with_code("E0506"),
                );
                return self.poison(span);
            }
            Category::Rvalue => {
                self.report(
                    Diagnostic::error("lvalue required as unary '&' operand", span)
                        .with_code("E0506"),
                );
                return self.poison(span);
            }
        }
        let ty = self.types.pointer(self.tast[operand].ty);
        let node = ExprKind::Unary { op: UnaryOp::AddrOf, operand };
        self.tast.expr(Expr::new(node, ty, Category::Rvalue), span)
    }

    /// `++operand` and the three others, which are one rule with two spellings of the word.
    fn increment(&mut self, op: UnaryOp, operand: ExprId, span: Span) -> ExprId {
        if self.is_poisoned(operand) {
            return self.poison(span);
        }
        let what = match op {
            UnaryOp::PreInc | UnaryOp::PostInc => "increment",
            _ => "decrement",
        };
        let ty = self.tast[operand].ty;
        let lvalue = matches!(self.tast[operand].category, Category::Lvalue | Category::Bitfield);
        if !lvalue || is_array(&self.types, ty) {
            self.report(
                Diagnostic::error(format!("lvalue required as {what} operand"), span)
                    .with_code("E0506"),
            );
            return self.poison(span);
        }
        if self.types.quals(ty).has(Qualifiers::CONST) {
            let read_only = self.read_only(operand);
            self.report(
                Diagnostic::error(format!("{what} of read-only {read_only}"), span)
                    .with_code("E0507"),
            );
            return self.poison(span);
        }
        if !is_arithmetic(&self.types, ty) && !is_pointer(&self.types, ty) {
            return self.wrong_operand(what, span);
        }
        let ty = self.conv().read_as(ty);
        self.tast.expr(Expr::new(ExprKind::Unary { op, operand }, ty, Category::Rvalue), span)
    }

    /// A binary operator, which is a different rule for almost every operator.
    fn binary(&mut self, op: BinaryOp, lhs: ast::ExprId, rhs: ast::ExprId, span: Span) -> ExprId {
        let lhs = self.expr(lhs);
        let rhs = self.expr(rhs);
        match op {
            BinaryOp::Mul | BinaryOp::Div => self.arithmetic_binary(op, lhs, rhs, false, span),
            BinaryOp::Rem | BinaryOp::BitAnd | BinaryOp::BitXor | BinaryOp::BitOr => {
                self.arithmetic_binary(op, lhs, rhs, true, span)
            }
            BinaryOp::Add | BinaryOp::Sub => self.additive(op, lhs, rhs, span),
            BinaryOp::Shl | BinaryOp::Shr => self.shift(op, lhs, rhs, span),
            BinaryOp::Lt
            | BinaryOp::Gt
            | BinaryOp::Le
            | BinaryOp::Ge
            | BinaryOp::Eq
            | BinaryOp::Ne => self.comparison(op, lhs, rhs, span),
            BinaryOp::LogAnd | BinaryOp::LogOr => {
                let lhs = self.condition(lhs, span);
                let rhs = self.condition(rhs, span);
                if self.is_poisoned(lhs) || self.is_poisoned(rhs) {
                    return self.poison(span);
                }
                let ty = self.int();
                let node = ExprKind::Binary { op, lhs, rhs };
                self.tast.expr(Expr::new(node, ty, Category::Rvalue), span)
            }
        }
    }

    /// An operator whose two operands are arithmetic, or integer where the operator says so.
    fn arithmetic_binary(
        &mut self,
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
        integers: bool,
        span: Span,
    ) -> ExprId {
        let lhs = self.value(lhs);
        let rhs = self.value(rhs);
        if self.is_poisoned(lhs) || self.is_poisoned(rhs) {
            return self.poison(span);
        }
        let ok = |checker: &Checker<'_>, id: ExprId| {
            let ty = checker.tast[id].ty;
            if integers {
                is_integer(&checker.types, ty)
            } else {
                is_arithmetic(&checker.types, ty)
            }
        };
        if !ok(self, lhs) || !ok(self, rhs) {
            return self.invalid_operands(op, lhs, rhs, span);
        }
        let (lhs, rhs) = self.conv().usual_arithmetic(lhs, rhs).expect("two arithmetic operands");
        let ty = self.tast[lhs].ty;
        self.tast.expr(Expr::new(ExprKind::Binary { op, lhs, rhs }, ty, Category::Rvalue), span)
    }

    /// `+` and `-`, which are three operators each wearing one spelling.
    fn additive(&mut self, op: BinaryOp, lhs: ExprId, rhs: ExprId, span: Span) -> ExprId {
        let lhs = self.value(lhs);
        let rhs = self.value(rhs);
        if self.is_poisoned(lhs) || self.is_poisoned(rhs) {
            return self.poison(span);
        }
        let (left, right) = (self.tast[lhs].ty, self.tast[rhs].ty);
        let (pointers, integers) = (
            (is_pointer(&self.types, left), is_pointer(&self.types, right)),
            (is_integer(&self.types, left), is_integer(&self.types, right)),
        );
        let ty = match op {
            BinaryOp::Add if pointers.0 && integers.1 => left,
            // `1 + p` is the same expression as `p + 1`, and the operands stay in the order
            // they were written, because the node says which one is the pointer by its type.
            BinaryOp::Add if integers.0 && pointers.1 => right,
            BinaryOp::Sub if pointers.0 && integers.1 => left,
            // Two pointers subtract to a `ptrdiff_t`, and only when they point at the same
            // thing. The qualifiers do not count, which is why `const char *` minus `char *`
            // is fine and `int *` minus `char *` is not.
            BinaryOp::Sub if pointers.0 && pointers.1 => {
                let (a, b) = (
                    pointee(&self.types, left).expect("a pointer"),
                    pointee(&self.types, right).expect("a pointer"),
                );
                let (a, b) = (self.types.unqualified(a), self.types.unqualified(b));
                if !compatible(&self.types, a, b) {
                    return self.invalid_operands(op, lhs, rhs, span);
                }
                self.ptrdiff()
            }
            _ if is_arithmetic(&self.types, left) && is_arithmetic(&self.types, right) => {
                let (lhs, rhs) =
                    self.conv().usual_arithmetic(lhs, rhs).expect("two arithmetic operands");
                let ty = self.tast[lhs].ty;
                let node = ExprKind::Binary { op, lhs, rhs };
                return self.tast.expr(Expr::new(node, ty, Category::Rvalue), span);
            }
            _ => return self.invalid_operands(op, lhs, rhs, span),
        };
        self.tast.expr(Expr::new(ExprKind::Binary { op, lhs, rhs }, ty, Category::Rvalue), span)
    }

    /// `<<` and `>>`, where the two operands are promoted apart rather than together.
    fn shift(&mut self, op: BinaryOp, lhs: ExprId, rhs: ExprId, span: Span) -> ExprId {
        let lhs = self.conv().promote(lhs);
        let rhs = self.conv().promote(rhs);
        if self.is_poisoned(lhs) || self.is_poisoned(rhs) {
            return self.poison(span);
        }
        if !is_integer(&self.types, self.tast[lhs].ty)
            || !is_integer(&self.types, self.tast[rhs].ty)
        {
            return self.invalid_operands(op, lhs, rhs, span);
        }
        // The type of a shift is the type of its left operand promoted, and the right operand
        // keeps its own. A compiler that runs the usual arithmetic conversions here makes
        // `1 << 1L` a `long`, which is wrong and is the classic version of this bug.
        let ty = self.tast[lhs].ty;
        self.tast.expr(Expr::new(ExprKind::Binary { op, lhs, rhs }, ty, Category::Rvalue), span)
    }

    /// A relational or equality operator, whose value is an `int` however it is written.
    fn comparison(&mut self, op: BinaryOp, lhs: ExprId, rhs: ExprId, span: Span) -> ExprId {
        let mut lhs = self.value(lhs);
        let mut rhs = self.value(rhs);
        if self.is_poisoned(lhs) || self.is_poisoned(rhs) {
            return self.poison(span);
        }
        let (left, right) = (self.tast[lhs].ty, self.tast[rhs].ty);
        if is_arithmetic(&self.types, left) && is_arithmetic(&self.types, right) {
            let converted =
                self.conv().usual_arithmetic(lhs, rhs).expect("two arithmetic operands");
            (lhs, rhs) = converted;
        } else if is_pointer(&self.types, left) && is_pointer(&self.types, right) {
            let (a, b) = (
                pointee(&self.types, left).expect("a pointer"),
                pointee(&self.types, right).expect("a pointer"),
            );
            let (a, b) = (self.types.unqualified(a), self.types.unqualified(b));
            let either_void = is_void(&self.types, a) || is_void(&self.types, b);
            if !either_void && !compatible(&self.types, a, b) {
                self.report(
                    Diagnostic::warning("comparison of distinct pointer types lacks a cast", span)
                        .with_code("E0517"),
                );
            }
            rhs = self.conv().to_type(rhs, left);
        } else if is_pointer(&self.types, left) && self.conv().is_null_pointer_constant(rhs) {
            rhs = self.conv().to_type(rhs, left);
        } else if is_pointer(&self.types, right) && self.conv().is_null_pointer_constant(lhs) {
            lhs = self.conv().to_type(lhs, right);
        } else if is_pointer(&self.types, left) && is_integer(&self.types, right) {
            self.report(
                Diagnostic::warning("comparison between pointer and integer", span)
                    .with_code("E0517"),
            );
            rhs = self.conv().to_type(rhs, left);
        } else if is_integer(&self.types, left) && is_pointer(&self.types, right) {
            self.report(
                Diagnostic::warning("comparison between pointer and integer", span)
                    .with_code("E0517"),
            );
            lhs = self.conv().to_type(lhs, right);
        } else {
            return self.invalid_operands(op, lhs, rhs, span);
        }
        let ty = self.int();
        self.tast.expr(Expr::new(ExprKind::Binary { op, lhs, rhs }, ty, Category::Rvalue), span)
    }

    /// `lhs = rhs`, and the compound assignments.
    fn assign(
        &mut self,
        op: Option<BinaryOp>,
        lhs: ast::ExprId,
        rhs: ast::ExprId,
        span: Span,
    ) -> ExprId {
        let lhs = self.expr(lhs);
        let rhs = self.expr(rhs);
        let rhs = self.value(rhs);
        if self.is_poisoned(lhs) || self.is_poisoned(rhs) {
            return self.poison(span);
        }
        let target = self.tast[lhs].ty;
        if !matches!(self.tast[lhs].category, Category::Lvalue | Category::Bitfield) {
            self.report(
                Diagnostic::error("lvalue required as left operand of assignment", span)
                    .with_code("E0506"),
            );
            return self.poison(span);
        }
        if is_array(&self.types, target) {
            self.report(
                Diagnostic::error("assignment to expression with array type", span)
                    .with_code("E0507"),
            );
            return self.poison(span);
        }
        if self.types.quals(target).has(Qualifiers::CONST) {
            let read_only = self.read_only(lhs);
            self.report(
                Diagnostic::error(format!("assignment of read-only {read_only}"), span)
                    .with_code("E0507"),
            );
            return self.poison(span);
        }
        let ty = self.conv().read_as(target);
        let Some(op) = op else {
            let rhs = self.assign_to(ty, rhs, span, Target::Assignment);
            let node = ExprKind::Assign { op: None, computation: ty, lhs, rhs };
            return self.tast.expr(Expr::new(node, ty, Category::Rvalue), span);
        };
        let Some((computation, rhs)) = self.computation(op, ty, rhs, span) else {
            return self.poison(span);
        };
        let node = ExprKind::Assign { op: Some(op), computation, lhs, rhs };
        self.tast.expr(Expr::new(node, ty, Category::Rvalue), span)
    }

    /// The type a compound assignment performs its operation in, and its converted right side.
    ///
    /// [`None`] where the operator does not accept the pair, having reported why.
    fn computation(
        &mut self,
        op: BinaryOp,
        target: TypeId,
        rhs: ExprId,
        span: Span,
    ) -> Option<(TypeId, ExprId)> {
        let source = self.tast[rhs].ty;
        let integers =
            matches!(op, BinaryOp::Rem | BinaryOp::BitAnd | BinaryOp::BitXor | BinaryOp::BitOr);
        match op {
            // `p += 1` is pointer arithmetic and the operation happens in the pointer's type,
            // with the right side promoted and left alone.
            BinaryOp::Add | BinaryOp::Sub
                if is_pointer(&self.types, target) && is_integer(&self.types, source) =>
            {
                let rhs = self.conv().promote(rhs);
                Some((target, rhs))
            }
            // A shift performs its operation in the promoted type of the left side, and the
            // right side is promoted on its own, exactly as the binary operator does.
            BinaryOp::Shl | BinaryOp::Shr
                if is_integer(&self.types, target) && is_integer(&self.types, source) =>
            {
                let rhs = self.conv().promote(rhs);
                let computation = rucc_types::promote(&mut self.types, target, self.cx.target);
                Some((computation, rhs))
            }
            _ => {
                let ok = if integers {
                    is_integer(&self.types, target) && is_integer(&self.types, source)
                } else {
                    is_arithmetic(&self.types, target) && is_arithmetic(&self.types, source)
                };
                if !ok {
                    let (left, right) = (self.spell(target), self.spell(source));
                    self.report(
                        Diagnostic::error(
                            format!(
                                "invalid operands to binary {} (have '{left}' and '{right}')",
                                op.spelling()
                            ),
                            span,
                        )
                        .with_code("E0508"),
                    );
                    return None;
                }
                let computation =
                    rucc_types::usual_arithmetic(&mut self.types, target, source, self.cx.target)
                        .expect("two arithmetic operands");
                let rhs = self.conv().to_type(rhs, computation);
                Some((computation, rhs))
            }
        }
    }

    /// `cond ? then : otherwise`, and GNU's form with the middle left out.
    fn conditional(
        &mut self,
        cond: ast::ExprId,
        then: Option<ast::ExprId>,
        otherwise: ast::ExprId,
        span: Span,
    ) -> ExprId {
        let cond = self.expr(cond);
        let cond = self.value(cond);
        // GNU's `a ?: b` evaluates `a` once and yields it when it is true, which the tree says
        // by having the second arm be the very node the condition was converted from.
        let then = match then {
            Some(then) => {
                let then = self.expr(then);
                self.value(then)
            }
            None => cond,
        };
        let otherwise = self.expr(otherwise);
        let otherwise = self.value(otherwise);
        let cond = self.condition(cond, span);
        if self.is_poisoned(cond) || self.is_poisoned(then) || self.is_poisoned(otherwise) {
            return self.poison(span);
        }
        let (left, right) = (self.tast[then].ty, self.tast[otherwise].ty);
        let (ty, then, otherwise) = if is_arithmetic(&self.types, left)
            && is_arithmetic(&self.types, right)
        {
            let (then, otherwise) =
                self.conv().usual_arithmetic(then, otherwise).expect("two arithmetic operands");
            (self.tast[then].ty, then, otherwise)
        } else if is_void(&self.types, left) && is_void(&self.types, right) {
            (self.types.void(), then, otherwise)
        } else if is_pointer(&self.types, left) && self.conv().is_null_pointer_constant(otherwise) {
            let otherwise = self.conv().to_type(otherwise, left);
            (left, then, otherwise)
        } else if is_pointer(&self.types, right) && self.conv().is_null_pointer_constant(then) {
            let then = self.conv().to_type(then, right);
            (right, then, otherwise)
        } else if is_pointer(&self.types, left) && is_pointer(&self.types, right) {
            let otherwise = self.conv().to_type(otherwise, left);
            (left, then, otherwise)
        } else if is_pointer(&self.types, left) && is_integer(&self.types, right) {
            self.report(
                Diagnostic::error("pointer/integer type mismatch in conditional expression", span)
                    .with_code("E0518"),
            );
            let otherwise = self.conv().to_type(otherwise, left);
            (left, then, otherwise)
        } else if is_integer(&self.types, left) && is_pointer(&self.types, right) {
            self.report(
                Diagnostic::error("pointer/integer type mismatch in conditional expression", span)
                    .with_code("E0518"),
            );
            let then = self.conv().to_type(then, right);
            (right, then, otherwise)
        } else if is_record(&self.types, left) && compatible(&self.types, left, right) {
            (left, then, otherwise)
        } else {
            let (left, right) = (self.spell(left), self.spell(right));
            self.report(
                Diagnostic::error(
                    format!("type mismatch in conditional expression, '{left}' and '{right}'"),
                    span,
                )
                .with_code("E0518"),
            );
            return self.poison(span);
        };
        let node = ExprKind::Cond { cond, then, otherwise };
        self.tast.expr(Expr::new(node, ty, Category::Rvalue), span)
    }

    /// `lhs, rhs`, whose left side is evaluated and thrown away.
    fn comma(&mut self, lhs: ast::ExprId, rhs: ast::ExprId, span: Span) -> ExprId {
        let lhs = self.expr(lhs);
        let lhs = self.value(lhs);
        let rhs = self.expr(rhs);
        let rhs = self.value(rhs);
        if self.is_poisoned(lhs) || self.is_poisoned(rhs) {
            return self.poison(span);
        }
        let ty = self.tast[rhs].ty;
        self.tast.expr(Expr::new(ExprKind::Comma { lhs, rhs }, ty, Category::Rvalue), span)
    }

    /// A value converted to the type it is being assigned to, argument passing included.
    pub(in crate::check) fn assign_to(
        &mut self,
        target: TypeId,
        value: ExprId,
        span: Span,
        to: Target,
    ) -> ExprId {
        if self.is_poisoned(value) {
            return value;
        }
        let source = self.tast[value].ty;
        let boolean = self.types.boolean();
        if is_arithmetic(&self.types, target) && is_arithmetic(&self.types, source) {
            self.warn_overflow(value, target);
            return self.conv().to_type(value, target);
        }
        if self.types.unqualified(target) == boolean && is_scalar(&self.types, source) {
            return self.conv().to_type(value, target);
        }
        if is_pointer(&self.types, target) {
            if self.conv().is_null_pointer_constant(value) {
                return self.conv().to_type(value, target);
            }
            if is_pointer(&self.types, source) {
                self.check_pointer_assignment(target, source, span, to);
                return self.conv().to_type(value, target);
            }
            if is_integer(&self.types, source) {
                self.bad_conversion(target, source, "pointer from integer", span, to);
                return self.conv().to_type(value, target);
            }
        }
        if is_integer(&self.types, target) && is_pointer(&self.types, source) {
            self.bad_conversion(target, source, "integer from pointer", span, to);
            return self.conv().to_type(value, target);
        }
        let (bare_target, bare_source) =
            (self.types.unqualified(target), self.types.unqualified(source));
        if compatible(&self.types, bare_target, bare_source) {
            return self.conv().to_type(value, target);
        }
        if is_void(&self.types, source) {
            self.report(
                Diagnostic::error("void value not ignored as it ought to be", span)
                    .with_code("E0516"),
            );
            return self.poison(span);
        }
        let message = match to {
            Target::Assignment => {
                let (target, source) = (self.spell(target), self.spell(source));
                format!("incompatible types when assigning to type '{target}' from type '{source}'")
            }
            Target::Argument { index, function } => {
                format!("incompatible type for argument {index}{}", self.of_function(function))
            }
            // gcc says `invalid initializer` where the object being built is an aggregate and
            // names both types where it is a scalar, which reads as one message about the
            // initializer and one about the value.
            Target::Initialization
                if is_record(&self.types, target) || is_array(&self.types, target) =>
            {
                "invalid initializer".to_owned()
            }
            Target::Initialization => {
                let (target, source) = (self.spell(target), self.spell(source));
                format!(
                    "incompatible types when initializing type '{target}' using type '{source}'"
                )
            }
            Target::Return => {
                let (target, source) = (self.spell(target), self.spell(source));
                format!(
                    "incompatible types when returning type '{source}' but '{target}' was expected"
                )
            }
        };
        self.report(Diagnostic::error(message, span).with_code("E0515"));
        self.poison(span)
    }

    /// What is wrong, if anything, with assigning one pointer to another.
    ///
    /// Dropping a qualifier is a warning and pointing somewhere else is an error, which is the
    /// split gcc 14 arrived at: losing a `const` breaks a promise the code made to itself, while
    /// an incompatible pointee is a type confusion the hardware will find later.
    fn check_pointer_assignment(&mut self, target: TypeId, source: TypeId, span: Span, to: Target) {
        let (a, b) = (
            pointee(&self.types, target).expect("a pointer"),
            pointee(&self.types, source).expect("a pointer"),
        );
        // The qualifiers are checked before the types are, because dropping a `const` is worth
        // saying even when the two point at the same thing, which is the common case.
        let (target_quals, source_quals) = (self.types.quals(a), self.types.quals(b));
        for (qual, name) in [
            (Qualifiers::CONST, "const"),
            (Qualifiers::VOLATILE, "volatile"),
            (Qualifiers::RESTRICT, "restrict"),
        ] {
            if source_quals.has(qual) && !target_quals.has(qual) {
                let what = match to {
                    Target::Assignment => "assignment".to_owned(),
                    Target::Argument { index, function } => {
                        format!("passing argument {index}{}", self.of_function(function))
                    }
                    Target::Initialization => "initialization".to_owned(),
                    Target::Return => "return".to_owned(),
                };
                self.report(
                    Diagnostic::warning(
                        format!("{what} discards '{name}' qualifier from pointer target type"),
                        span,
                    )
                    .with_code("E0514"),
                );
                return;
            }
        }
        let (a, b) = (self.types.unqualified(a), self.types.unqualified(b));
        // `void *` converts both ways without a word, which is what makes `malloc` usable
        // without a cast and what a compiler that warns here gets wrong.
        if is_void(&self.types, a) || is_void(&self.types, b) || compatible(&self.types, a, b) {
            return;
        }
        let message = match to {
            Target::Assignment => {
                let (target, source) = (self.spell(target), self.spell(source));
                format!("assignment to '{target}' from incompatible pointer type '{source}'")
            }
            Target::Argument { index, function } => {
                format!(
                    "passing argument {index}{} from incompatible pointer type",
                    self.of_function(function)
                )
            }
            Target::Initialization => {
                let (target, source) = (self.spell(target), self.spell(source));
                format!("initialization of '{target}' from incompatible pointer type '{source}'")
            }
            Target::Return => {
                let (target, source) = (self.spell(target), self.spell(source));
                format!(
                    "returning '{source}' from a function with incompatible return type '{target}'"
                )
            }
        };
        self.report(Diagnostic::error(message, span).with_code("E0512"));
    }

    /// The diagnostic for an integer meeting a pointer where a conversion was not asked for.
    ///
    /// An error rather than a warning, which is gcc 14's change and gcc 16's behaviour. It was a
    /// warning for thirty years and the code that relied on that is the code that breaks when a
    /// pointer is wider than an `int`, so the compilers agreed to stop accepting it.
    fn bad_conversion(
        &mut self,
        target: TypeId,
        source: TypeId,
        what: &str,
        span: Span,
        to: Target,
    ) {
        let message = match to {
            Target::Assignment => {
                let (target, source) = (self.spell(target), self.spell(source));
                format!("assignment to '{target}' from '{source}' makes {what} without a cast")
            }
            Target::Argument { index, function } => {
                format!(
                    "passing argument {index}{} makes {what} without a cast",
                    self.of_function(function)
                )
            }
            Target::Initialization => {
                let (target, source) = (self.spell(target), self.spell(source));
                format!("initialization of '{target}' from '{source}' makes {what} without a cast")
            }
            Target::Return => {
                let (target, source) = (self.spell(target), self.spell(source));
                format!(
                    "returning '{source}' from a function with return type '{target}' makes \
                     {what} without a cast"
                )
            }
        };
        self.report(Diagnostic::error(message, span).with_code("E0513"));
    }

    /// The warning for a constant that does not survive the conversion it is about to undergo.
    ///
    /// gcc's `-Woverflow`, and only for a conversion the language performed: `char c = 300;` is
    /// warned about and `(char)300` is not, because in the second the program said what it
    /// wanted. That is why this is here rather than in the folding, which cannot tell the two
    /// apart and should not have to.
    ///
    /// A conversion to a floating type is not warned about either. A number too large for a
    /// `float` becomes an infinity, which is a value the type has, and gcc says nothing about
    /// `float f = 1e300;`.
    fn warn_overflow(&mut self, value: ExprId, target: TypeId) {
        // `bool` is left out because converting to it is a comparison against zero and not a
        // truncation, so `bool b = 2;` loses nothing and gcc warns about neither.
        if matches!(eval::bare(&self.types, target), TypeKind::Bool) {
            return;
        }
        let Some(info) = eval::int_shape(&self.types, target, self.cx.target) else {
            return;
        };
        let source = self.tast[value].ty;
        let span = self.tast.expr_span(value);
        let Ok(folded) = self.eval_constant(value) else {
            return;
        };
        if !eval::overflows(folded, info) {
            return;
        }
        let what = if info.signed { "overflow in conversion" } else { "unsigned conversion" };
        let (from, to) = (self.spell(source), self.spell(target));
        let was = eval::spell_const(folded, eval::int_shape(&self.types, source, self.cx.target));
        let now = eval::spell_int(eval::narrowed(folded, info), info);
        let message =
            format!("{what} from '{from}' to '{to}' changes value from '{was}' to '{now}'");
        self.report(Diagnostic::warning(message, span).with_code("E0524"));
    }

    /// The default argument promotions, 6.5.2.2p6.
    ///
    /// What an argument gets where the prototype does not say what it should be: the integer
    /// promotions, and `float` widened to `double`. Both exist because of how varargs are read,
    /// and a compiler that forgets the second one passes four bytes where `va_arg` reads eight.
    fn default_promote(&mut self, arg: ExprId) -> ExprId {
        let arg = self.conv().promote(arg);
        let ty = self.tast[arg].ty;
        if self.types.kind(self.types.canonical(ty)) == TypeKind::Float(FloatKind::Float) {
            let double = self.types.float(FloatKind::Double);
            return self.conv().to_type(arg, double);
        }
        arg
    }

    /// An expression used as a condition, converted to `bool`.
    pub(in crate::check) fn condition(&mut self, expr: ExprId, span: Span) -> ExprId {
        let expr = self.value(expr);
        if self.is_poisoned(expr) {
            return expr;
        }
        let ty = self.tast[expr].ty;
        if is_scalar(&self.types, ty) {
            return self.conv().to_bool(expr);
        }
        if is_void(&self.types, ty) {
            self.report(
                Diagnostic::error("void value not ignored as it ought to be", span)
                    .with_code("E0516"),
            );
            return self.poison(span);
        }
        let what = self.type_word(ty);
        self.report(
            Diagnostic::error(format!("used {what} type value where scalar is required"), span)
                .with_code("E0510"),
        );
        self.poison(span)
    }

    /// Whether a type may be reached through a pointer, having reported it when it may not.
    ///
    /// A `void *` may be dereferenced, which is a warning and not an error, because gcc accepts
    /// it and real code relies on that. An incomplete record may not, and that is the message
    /// people actually hit, which is why it names the type.
    fn target_of_indirection(&mut self, ty: TypeId, span: Span) -> bool {
        if is_void(&self.types, ty) {
            self.report(
                Diagnostic::warning("dereferencing 'void *' pointer", span).with_code("E0520"),
            );
            return true;
        }
        if is_function(&self.types, ty) || is_complete(&self.types, ty) {
            return true;
        }
        let ty = self.spell(ty);
        self.report(
            Diagnostic::error(format!("invalid use of undefined type '{ty}'"), span)
                .with_code("E0503"),
        );
        false
    }

    /// The message for an operator that does not accept the types it was given.
    fn invalid_operands(&mut self, op: BinaryOp, lhs: ExprId, rhs: ExprId, span: Span) -> ExprId {
        let (left, right) = (self.spell(self.tast[lhs].ty), self.spell(self.tast[rhs].ty));
        self.report(
            Diagnostic::error(
                format!(
                    "invalid operands to binary {} (have '{left}' and '{right}')",
                    op.spelling()
                ),
                span,
            )
            .with_code("E0508"),
        );
        self.poison(span)
    }

    /// The message for a unary operator that does not accept what it was given.
    ///
    /// gcc does not name the type here, which is worth keeping: the message is about the
    /// operator and the caret is already under the operand.
    fn wrong_operand(&mut self, what: &str, span: Span) -> ExprId {
        self.report(
            Diagnostic::error(format!("wrong type argument to {what}"), span).with_code("E0509"),
        );
        self.poison(span)
    }

    /// An expression form that is recognised and not checked yet.
    fn unsupported(&mut self, what: &str, span: Span) -> ExprId {
        self.report(
            Diagnostic::error(format!("{what} is not supported yet"), span).with_code("E0519"),
        );
        self.poison(span)
    }

    /// A folded constant, as a node.
    fn constant(&mut self, value: Const, ty: TypeId, span: Span) -> ExprId {
        let value = self.tast.add_const(value);
        self.tast.expr(Expr::new(ExprKind::Const(value), ty, Category::Rvalue), span)
    }

    /// The value of an expression, which is what every operand but a few is.
    pub(in crate::check) fn value(&mut self, expr: ExprId) -> ExprId {
        self.conv().value(expr)
    }

    /// What a message calls the thing that cannot be written to.
    ///
    /// gcc names the variable or the member where it can and says `location` where it cannot,
    /// which is the case of `*p` and of anything else with no name to give.
    fn read_only(&self, expr: ExprId) -> String {
        match self.tast[expr].kind {
            ExprKind::Decl(decl) => match self.tast[decl].name {
                Some(name) => format!("variable '{}'", self.text(name)),
                None => "location".to_owned(),
            },
            ExprKind::Member { .. } => format!("member {}", self.field_name(expr)),
            _ => "location".to_owned(),
        }
    }

    /// How a member access names its member, quoted, and empty where the member has no name.
    fn field_name(&self, expr: ExprId) -> String {
        let ExprKind::Member { base, field } = self.tast[expr].kind else {
            return String::new();
        };
        let TypeKind::Record(record) = self.types.kind(self.types.canonical(self.tast[base].ty))
        else {
            return String::new();
        };
        match self.types.record_info(record).fields[field as usize].name {
            Some(name) => format!("'{}'", self.text(name)),
            None => String::new(),
        }
    }

    /// ` of 'f'`, or nothing where the call did not name a function.
    fn of_function(&self, function: Option<Symbol>) -> String {
        match function {
            Some(name) => format!(" of '{}'", self.text(name)),
            None => String::new(),
        }
    }

    /// What gcc calls a type in the message about a value that had to be a scalar.
    fn type_word(&self, ty: TypeId) -> &'static str {
        match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Record(record) => match self.types.record_info(record).kind {
                RecordKind::Struct => "struct",
                RecordKind::Union => "union",
            },
            TypeKind::Array { .. } => "array",
            TypeKind::Function(_) => "function",
            _ => "incomplete",
        }
    }

    /// `wchar_t`, which is an integer type the target picks.
    pub(in crate::check) fn wide_char(&self) -> TypeId {
        let target = self.cx.target;
        let kind = match (target.wchar_width, target.wchar_is_signed) {
            (16, false) => IntKind::UShort,
            (16, true) => IntKind::Short,
            (32, false) => IntKind::UInt,
            _ => IntKind::Int,
        };
        self.types.int(kind)
    }
}

/// The fixture the child module's tests use as well, which is why several of the helpers below
/// are visible outside this module.
#[cfg(test)]
mod tests {
    use rucc_ast::{BuiltinSet, DeclSpecs, DeclSpecsId, Declarator, DeclaratorId, Derived};
    use rucc_base::Interner;
    use rucc_base::float::{Float, Format};
    use rucc_lex::{CharConstant, FloatConstant, IntConstant, Remarks, StringLiteral};
    use rucc_session::Std;
    use rucc_target::{TargetInfo, Triple};
    use rucc_types::{FieldDecl, FunctionType, RecordOptions, layout_record};

    use super::*;
    use crate::check::Context;
    use crate::print::Printer;

    /// The untyped tree a test checks, built by hand.
    ///
    /// The names are interned here rather than in the checker because the checker borrows the
    /// interner for as long as it lives, which is the point at which a test stops being able to
    /// invent names. Everything a test needs to name is therefore named before it starts.
    pub(super) struct Fixture {
        pub(super) ast: rucc_ast::Ast,
        names: Interner,
        target: TargetInfo,
    }

    impl Fixture {
        pub(super) fn new() -> Fixture {
            Fixture::for_target("x86_64-unknown-linux-gnu")
        }

        /// The same, for a test whose answer is a property of the target.
        pub(super) fn for_target(triple: &str) -> Fixture {
            let target = TargetInfo::new(triple.parse::<Triple>().expect("a triple"));
            Fixture { ast: rucc_ast::Ast::new(), names: Interner::new(), target }
        }

        pub(super) fn name(&mut self, text: &str) -> Symbol {
            self.names.intern(text)
        }

        pub(super) fn expr(&mut self, expr: ast::Expr) -> ast::ExprId {
            self.ast.expr(expr, Span::DUMMY)
        }

        pub(super) fn use_name(&mut self, text: &str) -> ast::ExprId {
            let name = self.name(text);
            self.expr(ast::Expr::Name(name))
        }

        pub(super) fn int(&mut self, value: u128, kind: IntKind) -> ast::ExprId {
            let ty = IntConstantType::Standard(kind);
            let id = self.ast.add_int(IntConstant { value, ty, remarks: Remarks::default() });
            self.expr(ast::Expr::Int(id))
        }

        pub(super) fn one(&mut self) -> ast::ExprId {
            self.int(1, IntKind::Int)
        }

        pub(super) fn float(&mut self, text: &str) -> ast::ExprId {
            let (value, _) = Float::parse(text, Format::Double).expect("a float");
            let constant = FloatConstant {
                value,
                ty: FloatConstantType::Double,
                imaginary: false,
                remarks: Remarks::default(),
            };
            let id = self.ast.add_float(constant);
            self.expr(ast::Expr::Float(id))
        }

        fn binary(&mut self, op: BinaryOp, lhs: ast::ExprId, rhs: ast::ExprId) -> ast::ExprId {
            self.expr(ast::Expr::Binary { op, lhs, rhs })
        }

        fn unary(&mut self, op: UnaryOp, operand: ast::ExprId) -> ast::ExprId {
            self.expr(ast::Expr::Unary { op, operand })
        }

        fn assign(
            &mut self,
            op: Option<BinaryOp>,
            lhs: ast::ExprId,
            rhs: ast::ExprId,
        ) -> ast::ExprId {
            self.expr(ast::Expr::Assign { op, lhs, rhs })
        }

        fn call(&mut self, callee: ast::ExprId, args: &[ast::ExprId]) -> ast::ExprId {
            let args = self.ast.add_expr_list(args);
            self.expr(ast::Expr::Call { callee, args })
        }

        /// A specifier list naming a built-in type, as the keywords that were written.
        pub(super) fn keywords(&mut self, written: &[BuiltinSet]) -> DeclSpecsId {
            let mut builtin = rucc_ast::Builtin::NONE;
            for &keyword in written {
                builtin = builtin.add(keyword).expect("a keyword written once");
            }
            self.specs(ast::TypeSpec::Builtin(builtin))
        }

        pub(super) fn specs(&mut self, ty: ast::TypeSpec) -> DeclSpecsId {
            let mut specs = DeclSpecs::empty(Span::DUMMY);
            specs.ty = ty;
            self.ast.add_specs(specs)
        }

        /// A type name, which is a specifier list and an abstract declarator.
        pub(super) fn type_name(
            &mut self,
            specs: DeclSpecsId,
            derived: &[Derived],
        ) -> ast::TypeNameId {
            let declarator = self.declarator(derived);
            self.ast.add_type_name(ast::TypeName { specs, declarator, span: Span::DUMMY })
        }

        pub(super) fn declarator(&mut self, derived: &[Derived]) -> DeclaratorId {
            let derived = self.ast.add_derived_list(derived);
            self.ast.add_declarator(Declarator {
                name: None,
                name_span: Span::DUMMY,
                derived,
                span: Span::DUMMY,
            })
        }

        pub(super) fn checker(&self) -> Checker<'_> {
            Checker::new(&self.ast, Context::new(&self.names, &self.target, Std::C23))
        }
    }

    /// The tree under one node, which is what almost every assertion here is about.
    pub(super) fn dump(checker: &Checker<'_>, id: ExprId) -> String {
        let mut printer = Printer::new(&checker.tast, &checker.types, checker.cx.names);
        printer.expr(id);
        printer.finish()
    }

    /// What was reported, as the messages alone.
    pub(super) fn messages(checker: &Checker<'_>) -> Vec<String> {
        checker.errors.diagnostics().iter().map(|d| d.message.clone()).collect()
    }

    /// The one message that was reported, which is what most of these tests expect.
    pub(super) fn message(checker: &Checker<'_>) -> String {
        let mut reported = messages(checker);
        assert_eq!(reported.len(), 1, "expected exactly one diagnostic, got {reported:?}");
        reported.pop().expect("one message")
    }

    /// Declares a `struct` with the given members and gives back its type.
    pub(super) fn record(
        checker: &mut Checker<'_>,
        tag: Option<Symbol>,
        fields: &[FieldDecl],
    ) -> TypeId {
        let id = checker.types.declare_record(RecordKind::Struct, tag);
        let ty = checker.types.record(id);
        let laid_out = layout_record(
            &checker.types,
            RecordKind::Struct,
            fields,
            &RecordOptions::default(),
            checker.cx.target,
        )
        .expect("a layout");
        checker.types.complete_record(id, laid_out);
        ty
    }

    #[test]
    fn a_name_is_the_declaration_it_resolved_to_and_an_lvalue() {
        let mut f = Fixture::new();
        let x = f.name("x");
        let use_x = f.expr(ast::Expr::Name(x));

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        c.declare_object(x, int, Span::DUMMY);
        let id = c.check_expr(use_x);

        assert_eq!(dump(&c, id), "decl #0 x : int lvalue\n");
        assert!(c.errors.is_empty());
    }

    #[test]
    fn an_undeclared_name_is_reported_once_however_many_operators_use_it() {
        let mut f = Fixture::new();
        let x = f.use_name("x");
        let one = f.one();
        let sum = f.binary(BinaryOp::Add, x, one);
        let product = f.binary(BinaryOp::Mul, sum, one);

        let mut c = f.checker();
        let id = c.check_expr(product);

        assert_eq!(message(&c), "'x' undeclared (first use in this function)");
        assert_eq!(dump(&c, id), "error : int\n");
    }

    #[test]
    fn an_enumerator_is_the_number_and_not_a_use_of_anything() {
        let mut f = Fixture::new();
        let red = f.name("red");
        let use_red = f.expr(ast::Expr::Name(red));

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        c.scopes.declare(red, Binding::Enumerator { value: 3, ty: int });
        let id = c.check_expr(use_red);

        assert_eq!(dump(&c, id), "const 3 : int\n");
    }

    #[test]
    fn an_integer_constant_has_the_type_the_lexer_gave_it() {
        let mut f = Fixture::new();
        let value = f.int(7, IntKind::ULong);

        let mut c = f.checker();
        let id = c.check_expr(value);

        assert_eq!(dump(&c, id), "const 7 : unsigned long\n");
    }

    #[test]
    fn a_character_constant_is_an_int_because_this_is_not_cpp() {
        let mut f = Fixture::new();
        let constant =
            CharConstant { value: 97, encoding: Encoding::Plain, remarks: Remarks::default() };
        let id = f.ast.add_char(constant);
        let expr = f.expr(ast::Expr::Char(id));

        let mut c = f.checker();
        let id = c.check_expr(expr);

        assert_eq!(dump(&c, id), "const 97 : int\n");
    }

    #[test]
    fn a_string_literal_is_an_array_one_longer_than_it_looks() {
        let mut f = Fixture::new();
        let literal = StringLiteral {
            elements: vec![u32::from(b'h'), u32::from(b'i')],
            encoding: Encoding::Plain,
            remarks: Remarks::default(),
        };
        let id = f.ast.add_string(literal);
        let expr = f.expr(ast::Expr::Str(id));

        let mut c = f.checker();
        let id = c.check_expr(expr);

        assert_eq!(dump(&c, id), "string \"hi\" : char [3] lvalue\n");
    }

    #[test]
    fn an_array_decays_where_it_is_used_and_not_where_its_address_is_taken() {
        let mut f = Fixture::new();
        let a = f.name("a");
        let use_a = f.expr(ast::Expr::Name(a));
        let index = f.expr(ast::Expr::Name(a));
        let zero = f.int(0, IntKind::Int);
        let subscript = f.expr(ast::Expr::Index { base: use_a, index: zero });
        let address = f.unary(UnaryOp::AddrOf, index);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let array = c.types.array(int, ArrayLen::Fixed(4));
        c.declare_object(a, array, Span::DUMMY);
        let subscript = c.check_expr(subscript);
        let address = c.check_expr(address);

        assert_eq!(
            dump(&c, subscript),
            "subscript : int lvalue\n  convert array-decay : int *\n    decl #0 a : int [4] lvalue\n  const 0 : int\n"
        );
        assert_eq!(dump(&c, address), "unary & : int (*)[4]\n  decl #0 a : int [4] lvalue\n");
    }

    #[test]
    fn a_subscript_keeps_the_pointer_first_however_it_was_written() {
        let mut f = Fixture::new();
        let p = f.name("p");
        let use_p = f.expr(ast::Expr::Name(p));
        let zero = f.int(0, IntKind::Int);
        let backwards = f.expr(ast::Expr::Index { base: zero, index: use_p });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let pointer = c.types.pointer(int);
        c.declare_object(p, pointer, Span::DUMMY);
        let id = c.check_expr(backwards);

        let text = dump(&c, id);
        assert!(text.starts_with("subscript : int lvalue\n  convert lvalue : int *\n"), "{text}");
        assert!(text.ends_with("  const 0 : int\n"), "{text}");
    }

    #[test]
    fn a_subscript_of_two_integers_is_not_a_subscript() {
        let mut f = Fixture::new();
        let one = f.one();
        let two = f.int(2, IntKind::Int);
        let subscript = f.expr(ast::Expr::Index { base: one, index: two });

        let mut c = f.checker();
        c.check_expr(subscript);

        assert_eq!(message(&c), "subscripted value is neither array nor pointer nor vector");
    }

    #[test]
    fn a_call_converts_each_argument_to_what_the_prototype_asks_for() {
        let mut f = Fixture::new();
        let g = f.name("g");
        let use_g = f.expr(ast::Expr::Name(g));
        let one = f.one();
        let call = f.call(use_g, &[one]);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let long = c.types.int(IntKind::Long);
        let signature =
            FunctionType { ret: int, params: vec![long], variadic: false, prototyped: true };
        let function = c.types.function(signature);
        c.declare_object(g, function, Span::DUMMY);
        let id = c.check_expr(call);

        assert_eq!(
            dump(&c, id),
            "call : int\n  convert function-decay : int (*)(long)\n    decl #0 g : int (long) function\n  convert arithmetic : long\n    const 1 : int\n"
        );
    }

    #[test]
    fn a_variadic_argument_takes_the_default_promotions_because_va_arg_reads_them() {
        let mut f = Fixture::new();
        let g = f.name("g");
        let x = f.name("x");
        let use_g = f.expr(ast::Expr::Name(g));
        let one = f.one();
        let use_x = f.expr(ast::Expr::Name(x));
        let call = f.call(use_g, &[one, use_x]);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let float = c.types.float(FloatKind::Float);
        let signature =
            FunctionType { ret: int, params: vec![int], variadic: true, prototyped: true };
        let function = c.types.function(signature);
        c.declare_object(g, function, Span::DUMMY);
        c.declare_object(x, float, Span::DUMMY);
        let id = c.check_expr(call);

        assert!(c.errors.is_empty());
        assert!(dump(&c, id).contains("convert arithmetic : double\n"), "{}", dump(&c, id));
    }

    #[test]
    fn calling_something_that_is_not_a_function_names_what_was_called() {
        let mut f = Fixture::new();
        let x = f.name("x");
        let use_x = f.expr(ast::Expr::Name(x));
        let call = f.call(use_x, &[]);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        c.declare_object(x, int, Span::DUMMY);
        c.check_expr(call);

        assert_eq!(message(&c), "called object 'x' is not a function or function pointer");
    }

    #[test]
    fn the_argument_count_is_checked_against_the_prototype() {
        let mut f = Fixture::new();
        let g = f.name("g");
        let use_g = f.expr(ast::Expr::Name(g));
        let call = f.call(use_g, &[]);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let signature =
            FunctionType { ret: int, params: vec![int], variadic: false, prototyped: true };
        let function = c.types.function(signature);
        c.declare_object(g, function, Span::DUMMY);
        c.check_expr(call);

        assert_eq!(message(&c), "too few arguments to function 'g'");
    }

    #[test]
    fn an_arrow_is_a_dereference_and_then_a_member() {
        let mut f = Fixture::new();
        let p = f.name("p");
        let x = f.name("x");
        let use_p = f.expr(ast::Expr::Name(p));
        let member = f.expr(ast::Expr::Member { base: use_p, name: x, arrow: true });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let s = record(&mut c, None, &[FieldDecl::new(Some(x), int)]);
        let pointer = c.types.pointer(s);
        c.declare_object(p, pointer, Span::DUMMY);
        let id = c.check_expr(member);

        assert_eq!(
            dump(&c, id),
            "member #0 x : int lvalue\n  unary * : struct <anonymous> lvalue\n    convert lvalue : struct <anonymous> *\n      decl #0 p : struct <anonymous> * lvalue\n"
        );
    }

    #[test]
    fn a_member_of_a_const_object_is_const_whatever_it_was_declared_as() {
        let mut f = Fixture::new();
        let s = f.name("s");
        let x = f.name("x");
        let tag = f.name("S");
        let use_s = f.expr(ast::Expr::Name(s));
        let member = f.expr(ast::Expr::Member { base: use_s, name: x, arrow: false });
        let one = f.one();
        let assign = f.assign(None, member, one);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let record = record(&mut c, Some(tag), &[FieldDecl::new(Some(x), int)]);
        let constant = c.types.qualified(record, Qualifiers::CONST);
        c.declare_object(s, constant, Span::DUMMY);
        c.check_expr(assign);

        assert_eq!(message(&c), "assignment of read-only member 'x'");
    }

    #[test]
    fn a_member_of_an_anonymous_member_is_reached_through_the_member_that_holds_it() {
        let mut f = Fixture::new();
        let s = f.name("s");
        let x = f.name("x");
        let use_s = f.expr(ast::Expr::Name(s));
        let member = f.expr(ast::Expr::Member { base: use_s, name: x, arrow: false });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let inner = record(&mut c, None, &[FieldDecl::new(Some(x), int)]);
        let outer = record(&mut c, None, &[FieldDecl::new(None, inner)]);
        c.declare_object(s, outer, Span::DUMMY);
        let id = c.check_expr(member);

        let text = dump(&c, id);
        assert!(text.starts_with("member #0 x : int lvalue\n  member #0 : "), "{text}");
    }

    #[test]
    fn a_member_that_is_not_there_names_the_type_that_does_not_have_it() {
        let mut f = Fixture::new();
        let s = f.name("s");
        let x = f.name("x");
        let y = f.name("y");
        let tag = f.name("S");
        let use_s = f.expr(ast::Expr::Name(s));
        let member = f.expr(ast::Expr::Member { base: use_s, name: y, arrow: false });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let ty = record(&mut c, Some(tag), &[FieldDecl::new(Some(x), int)]);
        c.declare_object(s, ty, Span::DUMMY);
        c.check_expr(member);

        assert_eq!(message(&c), "'struct S' has no member named 'y'");
    }

    #[test]
    fn the_address_of_a_bit_field_is_the_one_thing_an_lvalue_cannot_give() {
        let mut f = Fixture::new();
        let s = f.name("s");
        let x = f.name("x");
        let use_s = f.expr(ast::Expr::Name(s));
        let member = f.expr(ast::Expr::Member { base: use_s, name: x, arrow: false });
        let address = f.unary(UnaryOp::AddrOf, member);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let ty = record(&mut c, None, &[FieldDecl::bit_field(Some(x), int, 3)]);
        c.declare_object(s, ty, Span::DUMMY);
        c.check_expr(address);

        assert_eq!(message(&c), "cannot take address of bit-field 'x'");
    }

    #[test]
    fn the_usual_arithmetic_conversions_are_written_into_the_tree() {
        let mut f = Fixture::new();
        let one = f.one();
        let long = f.int(2, IntKind::Long);
        let sum = f.binary(BinaryOp::Add, one, long);

        let mut c = f.checker();
        let id = c.check_expr(sum);

        assert_eq!(
            dump(&c, id),
            "binary + : long\n  convert arithmetic : long\n    const 1 : int\n  const 2 : long\n"
        );
    }

    #[test]
    fn a_shift_does_not_take_the_type_of_its_right_operand() {
        let mut f = Fixture::new();
        let one = f.one();
        let long = f.int(2, IntKind::Long);
        let shift = f.binary(BinaryOp::Shl, one, long);

        let mut c = f.checker();
        let id = c.check_expr(shift);

        assert_eq!(dump(&c, id), "binary << : int\n  const 1 : int\n  const 2 : long\n");
    }

    #[test]
    fn adding_an_integer_to_a_pointer_gives_the_pointer_back() {
        let mut f = Fixture::new();
        let p = f.name("p");
        let use_p = f.expr(ast::Expr::Name(p));
        let one = f.one();
        let sum = f.binary(BinaryOp::Add, one, use_p);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let pointer = c.types.pointer(int);
        c.declare_object(p, pointer, Span::DUMMY);
        let id = c.check_expr(sum);

        assert!(c.errors.is_empty());
        assert!(dump(&c, id).starts_with("binary + : int *\n"), "{}", dump(&c, id));
    }

    #[test]
    fn subtracting_two_pointers_gives_the_type_a_difference_fits_in() {
        let mut f = Fixture::new();
        let p = f.name("p");
        let left = f.expr(ast::Expr::Name(p));
        let right = f.expr(ast::Expr::Name(p));
        let difference = f.binary(BinaryOp::Sub, left, right);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let pointer = c.types.pointer(int);
        c.declare_object(p, pointer, Span::DUMMY);
        let id = c.check_expr(difference);

        assert!(c.errors.is_empty());
        assert!(dump(&c, id).starts_with("binary - : long\n"), "{}", dump(&c, id));
    }

    #[test]
    fn comparing_two_unrelated_pointers_is_worth_a_word() {
        let mut f = Fixture::new();
        let p = f.name("p");
        let q = f.name("q");
        let left = f.expr(ast::Expr::Name(p));
        let right = f.expr(ast::Expr::Name(q));
        let compare = f.binary(BinaryOp::Eq, left, right);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let char_ = c.types.int(IntKind::Char);
        let to_int = c.types.pointer(int);
        let to_char = c.types.pointer(char_);
        c.declare_object(p, to_int, Span::DUMMY);
        c.declare_object(q, to_char, Span::DUMMY);
        let id = c.check_expr(compare);

        assert_eq!(message(&c), "comparison of distinct pointer types lacks a cast");
        assert!(dump(&c, id).starts_with("binary == : int\n"), "{}", dump(&c, id));
    }

    #[test]
    fn a_logical_operator_converts_both_sides_to_bool() {
        let mut f = Fixture::new();
        let p = f.name("p");
        let left = f.expr(ast::Expr::Name(p));
        let one = f.one();
        let and = f.binary(BinaryOp::LogAnd, left, one);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let pointer = c.types.pointer(int);
        c.declare_object(p, pointer, Span::DUMMY);
        let id = c.check_expr(and);

        let text = dump(&c, id);
        assert!(text.starts_with("binary && : int\n"), "{text}");
        assert_eq!(text.matches("convert bool : _Bool").count(), 2, "{text}");
    }

    #[test]
    fn assigning_to_a_const_object_names_the_variable() {
        let mut f = Fixture::new();
        let x = f.name("x");
        let use_x = f.expr(ast::Expr::Name(x));
        let one = f.one();
        let assign = f.assign(None, use_x, one);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let constant = c.types.qualified(int, Qualifiers::CONST);
        c.declare_object(x, constant, Span::DUMMY);
        c.check_expr(assign);

        assert_eq!(message(&c), "assignment of read-only variable 'x'");
    }

    #[test]
    fn an_array_is_not_something_that_can_be_assigned_to() {
        let mut f = Fixture::new();
        let a = f.name("a");
        let use_a = f.expr(ast::Expr::Name(a));
        let one = f.one();
        let assign = f.assign(None, use_a, one);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let array = c.types.array(int, ArrayLen::Fixed(4));
        c.declare_object(a, array, Span::DUMMY);
        c.check_expr(assign);

        assert_eq!(message(&c), "assignment to expression with array type");
    }

    #[test]
    fn a_compound_assignment_performs_its_operation_in_the_type_the_operation_needs() {
        let mut f = Fixture::new();
        let i = f.name("i");
        let use_i = f.expr(ast::Expr::Name(i));
        let half = f.float("0.5");
        let divide = f.assign(Some(BinaryOp::Div), use_i, half);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        c.declare_object(i, int, Span::DUMMY);
        let id = c.check_expr(divide);

        assert!(c.errors.is_empty());
        let text = dump(&c, id);
        assert!(text.starts_with("assign /= in double : int\n"), "{text}");
    }

    #[test]
    fn assigning_an_unrelated_pointer_is_a_warning_because_gcc_accepts_it() {
        let mut f = Fixture::new();
        let p = f.name("p");
        let q = f.name("q");
        let left = f.expr(ast::Expr::Name(p));
        let right = f.expr(ast::Expr::Name(q));
        let assign = f.assign(None, left, right);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let char_ = c.types.int(IntKind::Char);
        let to_int = c.types.pointer(int);
        let to_char = c.types.pointer(char_);
        c.declare_object(p, to_int, Span::DUMMY);
        c.declare_object(q, to_char, Span::DUMMY);
        let id = c.check_expr(assign);

        assert_eq!(message(&c), "assignment to 'int *' from incompatible pointer type 'char *'");
        assert!(!c.is_poisoned(id));
    }

    #[test]
    fn dropping_const_from_what_a_pointer_points_at_is_worth_a_word() {
        let mut f = Fixture::new();
        let p = f.name("p");
        let q = f.name("q");
        let left = f.expr(ast::Expr::Name(p));
        let right = f.expr(ast::Expr::Name(q));
        let assign = f.assign(None, left, right);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let constant = c.types.qualified(int, Qualifiers::CONST);
        let to_int = c.types.pointer(int);
        let to_const = c.types.pointer(constant);
        c.declare_object(p, to_int, Span::DUMMY);
        c.declare_object(q, to_const, Span::DUMMY);
        c.check_expr(assign);

        assert_eq!(message(&c), "assignment discards 'const' qualifier from pointer target type");
    }

    #[test]
    fn a_pointer_and_an_integer_do_not_assign_without_a_cast() {
        let mut f = Fixture::new();
        let p = f.name("p");
        let left = f.expr(ast::Expr::Name(p));
        let one = f.one();
        let assign = f.assign(None, left, one);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let pointer = c.types.pointer(int);
        c.declare_object(p, pointer, Span::DUMMY);
        c.check_expr(assign);

        assert_eq!(
            message(&c),
            "assignment to 'int *' from 'int' makes pointer from integer without a cast"
        );
    }

    #[test]
    fn a_null_pointer_constant_assigns_to_any_pointer_without_a_word() {
        let mut f = Fixture::new();
        let p = f.name("p");
        let left = f.expr(ast::Expr::Name(p));
        let zero = f.int(0, IntKind::Int);
        let assign = f.assign(None, left, zero);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let pointer = c.types.pointer(int);
        c.declare_object(p, pointer, Span::DUMMY);
        let id = c.check_expr(assign);

        assert!(c.errors.is_empty());
        assert!(dump(&c, id).contains("convert null-pointer : int *\n"), "{}", dump(&c, id));
    }

    #[test]
    fn the_gnu_conditional_yields_the_condition_it_already_computed() {
        let mut f = Fixture::new();
        let x = f.name("x");
        let cond = f.expr(ast::Expr::Name(x));
        let one = f.one();
        let elided = f.expr(ast::Expr::Cond { cond, then: None, otherwise: one });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        c.declare_object(x, int, Span::DUMMY);
        let id = c.check_expr(elided);

        let ExprKind::Cond { cond, then, .. } = c.tast[id].kind else { panic!("a conditional") };
        let ExprKind::Convert { operand, .. } = c.tast[cond].kind else { panic!("a conversion") };
        assert_eq!(operand, then, "the middle operand is the condition and not a second read");
    }

    #[test]
    fn a_null_pointer_constant_in_a_conditional_takes_the_other_arms_type() {
        let mut f = Fixture::new();
        let p = f.name("p");
        let cond = f.one();
        let then = f.expr(ast::Expr::Name(p));
        let zero = f.int(0, IntKind::Int);
        let conditional = f.expr(ast::Expr::Cond { cond, then: Some(then), otherwise: zero });

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let pointer = c.types.pointer(int);
        c.declare_object(p, pointer, Span::DUMMY);
        let id = c.check_expr(conditional);

        assert!(c.errors.is_empty());
        assert_eq!(c.tast[id].ty, pointer);
    }

    #[test]
    fn a_record_where_a_scalar_is_required_says_which_kind_it_was() {
        let mut f = Fixture::new();
        let s = f.name("s");
        let x = f.name("x");
        let tag = f.name("S");
        let use_s = f.expr(ast::Expr::Name(s));
        let one = f.one();
        let and = f.binary(BinaryOp::LogAnd, use_s, one);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let ty = record(&mut c, Some(tag), &[FieldDecl::new(Some(x), int)]);
        c.declare_object(s, ty, Span::DUMMY);
        c.check_expr(and);

        assert_eq!(message(&c), "used struct type value where scalar is required");
    }

    #[test]
    fn incrementing_something_that_is_not_an_lvalue_is_refused() {
        let mut f = Fixture::new();
        let one = f.one();
        let increment = f.unary(UnaryOp::PreInc, one);

        let mut c = f.checker();
        c.check_expr(increment);

        assert_eq!(message(&c), "lvalue required as increment operand");
    }

    #[test]
    fn a_postfix_increment_has_the_type_the_object_reads_as() {
        let mut f = Fixture::new();
        let x = f.name("x");
        let use_x = f.expr(ast::Expr::Name(x));
        let increment = f.unary(UnaryOp::PostInc, use_x);

        let mut c = f.checker();
        let int = c.types.int(IntKind::Int);
        let volatile = c.types.qualified(int, Qualifiers::VOLATILE);
        c.declare_object(x, volatile, Span::DUMMY);
        let id = c.check_expr(increment);

        assert!(c.errors.is_empty());
        assert_eq!(dump(&c, id), "unary post ++ : int\n  decl #0 x : volatile int lvalue\n");
    }

    #[test]
    fn dereferencing_something_that_is_not_a_pointer_says_what_it_had() {
        let mut f = Fixture::new();
        let one = f.one();
        let deref = f.unary(UnaryOp::Deref, one);

        let mut c = f.checker();
        c.check_expr(deref);

        assert_eq!(message(&c), "invalid type argument of unary '*' (have 'int')");
    }

    #[test]
    fn a_form_that_waits_on_a_later_piece_is_refused_rather_than_guessed() {
        let mut f = Fixture::new();
        let (value, _) = Float::parse("1.0", Format::Double).expect("a float");
        let constant = FloatConstant {
            value,
            ty: FloatConstantType::Double,
            imaginary: true,
            remarks: Remarks::default(),
        };
        let value = f.ast.add_float(constant);
        let imaginary = f.expr(ast::Expr::Float(value));

        let mut c = f.checker();
        let id = c.check_expr(imaginary);

        assert_eq!(message(&c), "an imaginary constant is not supported yet");
        assert!(c.is_poisoned(id));
    }

    /// The type `1.0` written with the given suffix has on this target, as it would be written.
    fn suffixed(triple: &str, ty: FloatConstantType) -> String {
        let mut f = Fixture::for_target(triple);
        let (value, _) = Float::parse("1.0", Format::Double).expect("a float");
        let constant = FloatConstant { value, ty, imaginary: false, remarks: Remarks::default() };
        let id = f.ast.add_float(constant);
        let written = f.expr(ast::Expr::Float(id));

        let mut c = f.checker();
        let checked = c.check_expr(written);
        assert!(messages(&c).is_empty(), "{:?}", messages(&c));
        c.spell(c.tast[checked].ty)
    }

    #[test]
    fn a_floating_suffix_names_the_type_it_names_and_not_the_one_of_the_same_format() {
        // `1.0f32` is a `_Float32` and `1.0f` is a `float`. The two are binary32 either way and
        // are two types, so `_Generic` can tell them apart and the constant has to arrive
        // carrying the one that was written.
        let x86 = "x86_64-unknown-linux-gnu";
        assert_eq!(suffixed(x86, FloatConstantType::Float), "float");
        assert_eq!(suffixed(x86, FloatConstantType::Double), "double");
        assert_eq!(suffixed(x86, FloatConstantType::LongDouble), "long double");
        assert_eq!(suffixed(x86, FloatConstantType::Float16), "_Float16");
        assert_eq!(suffixed(x86, FloatConstantType::Float32), "_Float32");
        assert_eq!(suffixed(x86, FloatConstantType::Float64), "_Float64");
        assert_eq!(suffixed(x86, FloatConstantType::Float128), "_Float128");
        assert_eq!(suffixed(x86, FloatConstantType::Float32x), "_Float32x");
        assert_eq!(suffixed(x86, FloatConstantType::Float64x), "_Float64x");
        // `1.0w` is the x87 type, which on this target is what `long double` is, and gcc makes
        // them one type rather than two of the same format.
        assert_eq!(suffixed(x86, FloatConstantType::Float80), "long double");
    }

    /// Assigns a constant to an object of the given type and gives back what was said.
    fn narrowing(kind: IntKind, value: u128, constant: IntKind) -> Vec<String> {
        let mut f = Fixture::new();
        let c = f.name("c");
        let target = f.expr(ast::Expr::Name(c));
        let source = f.int(value, constant);
        let assignment = f.assign(None, target, source);

        let mut checker = f.checker();
        let ty = checker.types.int(kind);
        checker.declare_object(c, ty, Span::DUMMY);
        checker.check_expr(assignment);
        messages(&checker)
    }

    #[test]
    fn a_constant_that_does_not_survive_an_assignment_is_warned_about() {
        assert_eq!(
            narrowing(IntKind::Char, 300, IntKind::Int),
            ["overflow in conversion from 'int' to 'char' changes value from '300' to '44'"]
        );
        assert_eq!(
            narrowing(IntKind::UChar, 300, IntKind::Int),
            ["unsigned conversion from 'int' to 'unsigned char' changes value from '300' to '44'"]
        );
    }

    #[test]
    fn a_constant_that_only_changes_sign_is_not_an_overflow() {
        // Measured against gcc 13.3, which warns about neither. Two hundred is eight bits and
        // minus one is eight bits, and in each the bits all arrive: what moved is where the
        // sign is read, and that is a different option's business.
        assert!(narrowing(IntKind::SChar, 200, IntKind::Int).is_empty());
        assert!(narrowing(IntKind::UInt, 1, IntKind::Int).is_empty());
        assert!(narrowing(IntKind::Int, 4_294_967_295, IntKind::UInt).is_empty());
    }

    #[test]
    fn a_constant_that_widens_is_not_warned_about() {
        assert!(narrowing(IntKind::Long, 300, IntKind::Int).is_empty());
        assert!(narrowing(IntKind::Char, 100, IntKind::Int).is_empty());
    }

    #[test]
    fn an_explicit_conversion_to_bool_is_not_a_truncation_and_never_overflows() {
        let mut f = Fixture::new();
        let b = f.name("b");
        let target = f.expr(ast::Expr::Name(b));
        let source = f.int(2, IntKind::Int);
        let assignment = f.assign(None, target, source);

        let mut c = f.checker();
        let ty = c.types.boolean();
        c.declare_object(b, ty, Span::DUMMY);
        c.check_expr(assignment);

        assert!(messages(&c).is_empty(), "{:?}", messages(&c));
    }

    #[test]
    fn the_checking_hands_back_the_tree_the_types_and_what_went_wrong() {
        let mut f = Fixture::new();
        let x = f.use_name("x");

        let mut c = f.checker();
        c.check_expr(x);
        let checked = c.finish();

        assert!(checked.failed());
        assert_eq!(checked.diagnostics.len(), 1);
        assert_eq!(checked.diagnostics[0].code, Some("E0500"));
        assert_eq!(checked.tast.counts().exprs, 1);
    }
}
