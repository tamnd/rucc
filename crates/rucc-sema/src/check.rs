//! The pass: what walks the untyped tree and builds the typed one.
//!
//! Design: `spec/07-types-and-semantics.md`.
//!
//! The shape is the parser's, because the job is the same shape: a context of the things that do
//! not change, a walk that holds the things that do, and one structure handed back at the end.
//! What is different is that this walk has two trees, one it reads and one it writes, and the
//! reason a node is copied across rather than annotated in place is that the two are not the
//! same tree. A `p->x` is one node in the source and two here, an `int` meeting a `long` is
//! three, and an array used as a pointer is a node that the source does not contain at all.
//!
//! # What is here so far
//!
//! Expressions, which is where the constraints of 6.5 live. The ones that name a type are in
//! `check/expr/typeop.rs` and the rest are in `check/expr.rs`, which is a split by what the two
//! do rather than by size: an operator that names a type asks the type builder a question first
//! and most of them answer with a constant. The two that build an object rather than producing a
//! value, which are the compound literal and GNU's cast to a union type, are in `check/init.rs`
//! with the rest of initialization.
//!
//! Declarations, in `check/decl.rs`, which is what decides the linkage, the storage duration and
//! the definition state of each name and what reconciles the declarations that share one. The
//! type a declaration declares comes from `check/ty.rs`, through [`Checker::declared_type`] and
//! [`Checker::type_name`], which fold a declarator onto the type a specifier list named.
//!
//! Statements, in `check/stmt.rs`, which is the one walk here that carries state: what encloses
//! a statement is what decides whether it is allowed. The function definition is there too, since
//! a body is the only thing a statement list is ever part of, and so is [`Checker::check_unit`],
//! which walks a whole translation unit.
//!
//! Initialization, in `check/init.rs`, which turns the tree an initializer was parsed into
//! into a flat list of what goes where. It is its own module because it is its own algorithm:
//! a cursor over the object being initialized rather than a walk over the source, which is what
//! makes brace elision, designation and a string literal filling an array all the same thing
//! seen from different places. The compound literal is there too, since an unnamed object with
//! an initializer is what it is.
//!
//! Folding is reachable from here through [`Checker::eval_constant`] and
//! [`Checker::eval_integer`], and the checking asks for it in seven places: a narrowing
//! conversion that changes the value, an `alignas`, a `static_assert`, the initializer of a
//! `constexpr` object, a case label, the index of a designation, and each element of an
//! initializer for an object that exists before the program runs.
//!
//! # Poisoning
//!
//! The rule is the parser's, in `spec/06-lexer-and-parser.md` section 6.8, and it is the same
//! rule for the same reason. An expression that has been diagnosed becomes
//! [`ExprKind::Error`](crate::ExprKind::Error), and an operator whose operand is poisoned is
//! poisoned in turn without a word said about it. That is what keeps one undeclared name from
//! producing an error for every operator it appears under, and it is why nothing below asks
//! whether an error has already been reported: it asks whether the node in its hand is one.

use rucc_ast::Ast;
use rucc_base::{Interner, Symbol};
use rucc_diag::{DEFAULT_ERROR_LIMIT, Diagnostic, Errors, Span};
use rucc_session::Std;
use rucc_target::TargetInfo;
use rucc_types::{ArrayLen, IntKind, TypeId, TypeKind, Types, int_width};

use crate::convert::Conv;
use crate::decl::{Decl, DeclId, DeclKind, DeclList, Definition, Linkage, StorageDuration};
use crate::eval::{Eval, NotConstant};
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::scope::Scopes;
use crate::tast::{Const, Tast};

mod attr;
mod builtin;
mod decl;
mod expr;
mod init;
mod stmt;
mod ty;

/// What the checking needs and does not change.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// The spellings, for the diagnostics that name an identifier.
    pub names: &'a Interner,
    /// What the target's types are, which every layout and every promotion is decided by.
    pub target: &'a TargetInfo,
    /// The dialect.
    pub std: Std,
    /// Whether the GNU extensions are on.
    pub gnu: bool,
    /// Whether `-pedantic` was given.
    pub pedantic: bool,
    /// How many errors to report before stopping, with zero meaning no limit.
    pub error_limit: usize,
}

impl<'a> Context<'a> {
    /// A context with the defaults, for a caller that has an interner and a target to hand.
    #[must_use]
    pub fn new(names: &'a Interner, target: &'a TargetInfo, std: Std) -> Context<'a> {
        Context { names, target, std, gnu: true, pedantic: false, error_limit: DEFAULT_ERROR_LIMIT }
    }
}

/// What one run of the checking produced.
#[derive(Debug)]
pub struct Checked {
    /// The typed tree, which holds poisoned nodes where the source did not check.
    pub tast: Tast,
    /// The types, which the tree points into and which outlive it.
    pub types: Types,
    /// What went wrong, in the order it was found.
    pub diagnostics: Vec<Diagnostic>,
}

impl Checked {
    /// Whether anything was reported at an error severity.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.diagnostics.iter().any(|d| d.severity.is_fatal())
    }
}

/// The checking pass.
#[derive(Debug)]
pub struct Checker<'a> {
    pub(crate) ast: &'a Ast,
    pub(crate) tast: Tast,
    pub(crate) types: Types,
    pub(crate) scopes: Scopes,
    pub(crate) errors: Errors,
    pub(crate) cx: Context<'a>,
    /// What the type builder has already worked out, which is in `check/ty.rs` with the code
    /// that fills it in.
    pub(crate) built: ty::Built,
    /// The function body being checked, absent everywhere else. What is in it is in
    /// `check/stmt.rs`, which is the only code that reads it.
    pub(in crate::check) body: Option<stmt::Body>,
    /// The declarations whose initializers are being checked and whose types or values are not
    /// known until that finishes, which is what C23 calls underspecified. A name is in scope
    /// inside its own initializer, so this is what tells a reference to one from a use of the
    /// object it will become. Nested, because a statement expression may declare another.
    pub(in crate::check) underspecified: Vec<DeclId>,
}

impl<'a> Checker<'a> {
    /// A checker over one untyped tree.
    #[must_use]
    pub fn new(ast: &'a Ast, cx: Context<'a>) -> Checker<'a> {
        Checker {
            ast,
            tast: Tast::new(),
            types: Types::new(),
            scopes: Scopes::new(),
            errors: Errors::new(cx.error_limit),
            cx,
            built: ty::Built::default(),
            body: None,
            underspecified: Vec::new(),
        }
    }

    /// Checks a whole translation unit, which is what a compilation does.
    ///
    /// The declarations are checked in the order they were written, since that is the order the
    /// scopes are built in and the order the diagnostics belong in.
    pub fn check_unit(&mut self) {
        // Copied out because it is a shared reference with the checker's own lifetime, so holding
        // it does not borrow the checker that each declaration is checked through.
        let ast = self.ast;
        for &decl in ast.top_level() {
            self.check_decl(decl);
        }
    }

    /// Checks one expression and gives back the node it became.
    ///
    /// Always gives back a node. An expression that does not check is poisoned rather than
    /// absent, so that the operators around it are still checked and the diagnostics they would
    /// produce are still held back.
    pub fn check_expr(&mut self, id: rucc_ast::ExprId) -> ExprId {
        self.expr(id)
    }

    /// Folds a checked expression, reporting whatever the folding itself found wrong.
    ///
    /// # Errors
    ///
    /// [`NotConstant`] when the expression is not one. It is handed back rather than reported
    /// because the message names the context: `case label does not reduce to an integer
    /// constant` and `enumerator value for 'x' is not an integer constant` are two sentences
    /// about the same failure, and only the caller knows which one to write.
    pub fn eval_constant(&mut self, expr: ExprId) -> Result<Const, NotConstant> {
        let mut eval = self.eval();
        let value = eval.constant(expr);
        self.absorb(eval.finish());
        value
    }

    /// The same, for a context that needs an integer constant expression.
    ///
    /// # Errors
    ///
    /// [`NotConstant`] when the expression is not one, or is a constant of some other type.
    pub fn eval_integer(&mut self, expr: ExprId) -> Result<i128, NotConstant> {
        let mut eval = self.eval();
        let value = eval.integer(expr);
        self.absorb(eval.finish());
        value
    }

    /// The tree, the types and the diagnostics.
    #[must_use]
    pub fn finish(self) -> Checked {
        Checked { tast: self.tast, types: self.types, diagnostics: self.errors.finish() }
    }

    /// Declares an object in the current scope without a declaration to read it from.
    ///
    /// [`Checker::check_decl`] is what a translation unit goes through. This is for the caller
    /// that wants to check one expression against names it has decided on itself, which is what
    /// [`Checker::check_expr`] is for and what the tests here are built on.
    pub fn declare_object(&mut self, name: Symbol, ty: TypeId, span: Span) -> DeclId {
        let kind = if rucc_types::is_function(&self.types, ty) {
            DeclKind::Function
        } else {
            DeclKind::Object
        };
        let decl = self.tast.decl(
            Decl {
                name: Some(name),
                ty,
                kind,
                linkage: Linkage::None,
                duration: StorageDuration::Automatic,
                state: Definition::Defined,
                alignment: None,
                constant: false,
                init: None,
                params: DeclList::EMPTY,
                body: None,
            },
            span,
        );
        self.scopes.declare(name, crate::scope::Binding::Decl(decl));
        decl
    }

    /// The conversions, over this tree and these types.
    pub(crate) fn conv(&mut self) -> Conv<'_> {
        // The target is copied out first because it is a shared reference living as long as the
        // context, so taking it does not borrow the checker the two mutable ones are taken from.
        let target = self.cx.target;
        Conv { tast: &mut self.tast, types: &mut self.types, target }
    }

    /// The constant folding, over this tree and these types.
    pub(crate) fn eval(&self) -> Eval<'_> {
        Eval::new(&self.tast, &self.types, self.cx.target, self.cx.names)
    }

    /// Reports a diagnostic.
    pub(crate) fn report(&mut self, diagnostic: Diagnostic) {
        self.errors.push(diagnostic);
    }

    /// Reports everything the folding found, which it collects rather than pushing itself
    /// because it holds the tree while it runs and the error list is beside the tree.
    pub(crate) fn absorb(&mut self, diagnostics: Vec<Diagnostic>) {
        for diagnostic in diagnostics {
            self.errors.push(diagnostic);
        }
    }

    /// Whether a checked expression is one that was already the subject of a diagnostic.
    pub(crate) fn is_poisoned(&self, id: ExprId) -> bool {
        matches!(self.tast[id].kind, ExprKind::Error)
    }

    /// A poisoned expression, for the operand that did not check.
    ///
    /// Its type is `int` because every node has a type and there is no type meaning "no idea".
    /// Nothing reads it, since every operator that meets a poisoned operand poisons itself
    /// before it looks at what type the operand had.
    pub(crate) fn poison(&mut self, span: Span) -> ExprId {
        let int = self.types.int(IntKind::Int);
        self.tast.expr(Expr::new(ExprKind::Error, int, Category::Rvalue), span)
    }

    /// How a type is written, for a diagnostic that names one.
    pub(crate) fn spell(&self, ty: TypeId) -> String {
        rucc_types::spell(&self.types, self.cx.names, ty)
    }

    /// What a name is spelled, for a diagnostic that quotes one.
    pub(crate) fn text(&self, name: Symbol) -> &str {
        self.cx.names.resolve(name)
    }

    /// `int`, which is the type of every comparison and of `!`.
    pub(crate) fn int(&self) -> TypeId {
        self.types.int(IntKind::Int)
    }

    /// The type `sizeof` and `alignof` answer in, and the one an offset is measured in.
    ///
    /// Derived the same way [`Checker::ptrdiff`] is and for the same reason, since `size_t` is
    /// the unsigned type as wide as a pointer on every target this compiles for and asking the
    /// widths keeps the two from disagreeing about which one that is.
    pub(crate) fn size_type(&self) -> TypeId {
        let width = self.cx.target.pointer_width;
        for kind in [IntKind::UInt, IntKind::ULong, IntKind::ULongLong] {
            if int_width(kind, self.cx.target) >= width {
                return self.types.int(kind);
            }
        }
        self.types.int(IntKind::ULongLong)
    }

    /// Whether a type's size is worked out where it is reached rather than here.
    ///
    /// True for an array whose length is an expression, however deep it is: `int a[n][3]` is one
    /// and so is `int a[3][n]`. Shared between the operator that measures a type and the
    /// declaration that has to decide whether the object can live anywhere but the stack.
    pub(crate) fn is_variable_length(&self, ty: TypeId) -> bool {
        match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Array { elem, len } => {
                matches!(len, ArrayLen::Variable(_)) || self.is_variable_length(elem)
            }
            _ => false,
        }
    }

    /// Whether a type is variably modified, which is a variable length array or anything built
    /// out of one.
    ///
    /// `int a[n]` is one and so is `int (*p)[n]`, which is where this differs from
    /// [`Checker::is_variable_length`]: the pointer has the size every pointer has, and the
    /// thing it points at has a size the program worked out where the declaration was. That is
    /// why C says a jump may not enter the scope of either of them.
    pub(crate) fn is_variably_modified(&self, ty: TypeId) -> bool {
        match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Array { elem, len } => {
                matches!(len, ArrayLen::Variable(_)) || self.is_variably_modified(elem)
            }
            TypeKind::Pointer(pointee) => self.is_variably_modified(pointee),
            _ => false,
        }
    }

    /// The type of the difference between two pointers.
    ///
    /// Derived rather than stored, because `ptrdiff_t` is whatever signed type is as wide as a
    /// pointer and that is `long` on every LP64 target and `long long` on Windows, which is the
    /// same fact `long_width` already records. Asking the widths keeps the two from disagreeing.
    pub(crate) fn ptrdiff(&self) -> TypeId {
        let width = self.cx.target.pointer_width;
        for kind in [IntKind::Int, IntKind::Long, IntKind::LongLong] {
            if int_width(kind, self.cx.target) >= width {
                return self.types.int(kind);
            }
        }
        self.types.int(IntKind::LongLong)
    }
}
