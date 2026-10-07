//! `__attribute__((null_terminated_string_arg(n)))`, gcc 14's word that the `n`th argument of a
//! function is a string that ends in a zero byte, or a null pointer.
//!
//! gcc hands the promise to `-fanalyzer`, which checks a call against it, and to nothing else.
//! This compiler has no analyzer, so nothing reads it past here. What is left is gcc's checking of
//! the number, which a header written for gcc 14 meets, in its words: the number names a parameter
//! the way `alloc_size`'s and `nonnull`'s do, through gcc's `positional_argument`, and the
//! parameter it names has to be a pointer.

use rucc_ast::{AttrArg, AttrList, Attribute};
use rucc_diag::Diagnostic;
use rucc_types::{FunctionId, IntKind, TypeId, TypeKind};

use crate::check::Checker;
use crate::decl::DeclKind;
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::scope::Binding;

/// What one argument of the attribute turned out to be, before it is held against the function.
pub(in crate::check) enum Position {
    /// An integer constant, with how gcc prints it, suffix and all.
    Number(i128, String),
    /// Something gcc has already said what is wrong with.
    Refused,
}

/// Which argument of which attribute a position is, which is how gcc's `positional_argument`
/// starts everything it says about one: an attribute that takes more than one names the argument
/// by its number, and one that takes a single argument does not.
#[derive(Clone, Copy)]
pub(in crate::check) struct Lead {
    /// The attribute, as gcc spells it.
    pub name: &'static str,
    /// Which of its arguments, counted from one, for an attribute that takes more than one.
    pub argno: Option<usize>,
}

impl Lead {
    /// The words a diagnostic about the argument starts with.
    fn words(self) -> String {
        match self.argno {
            Some(argno) => format!("'{}' attribute argument {argno}", self.name),
            None => format!("'{}' attribute argument", self.name),
        }
    }
}

/// What the parameter a position names has to be.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::check) enum Wanted {
    /// A pointer, which is gcc's `POINTER_TYPE`.
    Pointer,
    /// An integer other than `bool`, which is gcc's `INTEGER_TYPE` as `positional_argument` reads
    /// it: an enumeration and a character type are integers there and a `bool` is not.
    Integer,
}

impl Checker<'_> {
    /// What gcc says about every `null_terminated_string_arg` in the lists of one declaration of
    /// that type.
    ///
    /// A function and a pointer to one are what it is for, since gcc applies an attribute that
    /// wants a function type to what the pointer points at. On any other type gcc warns that it
    /// only applies to function types. A list that passes is dropped all the same, there being
    /// nothing to keep it for.
    pub(in crate::check) fn string_arg(&mut self, lists: &[AttrList], ty: TypeId) {
        let ast = self.ast;
        for &attrs in lists {
            for &attr in &ast[attrs] {
                if self.gnu_name(&attr) != "null_terminated_string_arg" {
                    continue;
                }
                let args = &ast[attr.args];
                if args.len() != 1 {
                    let what = "wrong number of arguments specified for \
                                'null_terminated_string_arg' attribute";
                    let refused = Diagnostic::error(what, attr.span).with_code("E0839");
                    let note = format!("expected 1, found {}", args.len());
                    self.report(refused.note(note, attr.span));
                    continue;
                }
                let Some(function) = self.function_type(ty) else {
                    let what = "'null_terminated_string_arg' attribute only applies to function \
                                types";
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                    continue;
                };
                let lead = Lead { name: "null_terminated_string_arg", argno: None };
                if let Position::Number(value, spelled) = self.position(attr, args[0], lead) {
                    self.names_a(attr, function, value, &spelled, lead, Wanted::Pointer);
                }
            }
        }
    }

    /// The function a declaration of that type declares, or the one a pointer of it points at.
    pub(in crate::check) fn function_type(&self, ty: TypeId) -> Option<FunctionId> {
        let function = |ty: TypeId| match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Function(id) => Some(id),
            _ => None,
        };
        match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Pointer(pointee) => function(pointee),
            _ => function(ty),
        }
    }

    /// The number the argument is, after the conversions gcc applies to it first, or what gcc
    /// says about one that is not a number at all.
    ///
    /// A lone name stays a name in the parser, so it is looked up here: an enumerator is its
    /// value, an object or a function is the expression it would be, and a name nothing declared
    /// is gcc's error about it and then its warning that the argument is invalid.
    pub(in crate::check) fn position(
        &mut self,
        attr: Attribute,
        arg: AttrArg,
        lead: Lead,
    ) -> Position {
        let invalid = |checker: &mut Self| {
            let what = format!("{} is invalid", lead.words());
            checker.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
            Position::Refused
        };
        let (expr, named) = match arg {
            AttrArg::Expr(expr) => {
                let named = match self.ast[expr] {
                    rucc_ast::Expr::Name(name) => Some(self.text(name).to_owned()),
                    _ => None,
                };
                (self.expr(expr), named)
            }
            AttrArg::Ident(name) => match self.scopes.lookup(name) {
                Some(Binding::Enumerator { value, ty }) => {
                    let ty = rucc_types::promote(&mut self.types, ty, self.cx.target);
                    return Position::Number(value, format!("{value}{}", self.suffix(ty)));
                }
                Some(Binding::Decl(decl)) => {
                    let ty = self.tast[decl].ty;
                    let category = match self.tast[decl].kind {
                        DeclKind::Function => Category::Function,
                        DeclKind::Object | DeclKind::Type => Category::Lvalue,
                    };
                    let expr = Expr::new(ExprKind::Decl(decl), ty, category);
                    (self.tast.expr(expr, attr.span), Some(self.text(name).to_owned()))
                }
                Some(Binding::Typedef(_)) => return invalid(self),
                None => {
                    let spelled = self.text(name);
                    let what = if self.scopes.at_file_scope() {
                        format!("'{spelled}' undeclared here (not in a function)")
                    } else {
                        format!("'{spelled}' undeclared (first use in this function)")
                    };
                    self.report(Diagnostic::error(what, attr.span).with_code("E0500"));
                    return invalid(self);
                }
            },
        };
        self.number(attr, expr, named, lead)
    }

    /// The number an argument that checked is, promoted the way gcc promotes it first.
    fn number(
        &mut self,
        attr: Attribute,
        expr: ExprId,
        named: Option<String>,
        lead: Lead,
    ) -> Position {
        if matches!(self.tast[expr].kind, ExprKind::Error) {
            let what = format!("{} is invalid", lead.words());
            self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
            return Position::Refused;
        }
        let value = self.conv().promote(expr);
        let ty = self.tast[value].ty;
        if !rucc_types::is_integer(&self.types, self.types.canonical(ty)) {
            let what = format!("{} has type {}", lead.words(), self.gcc_quoted(ty));
            self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
            return Position::Refused;
        }
        let Ok(number) = self.eval_integer(value) else {
            let quoted = named.map_or_else(String::new, |named| format!(" '{named}'"));
            let what = format!("{} value{quoted} is not an integer constant", lead.words());
            self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
            return Position::Refused;
        };
        Position::Number(number, format!("{number}{}", self.suffix(ty)))
    }

    /// What gcc writes after an integer constant of that type when it prints one.
    fn suffix(&self, ty: TypeId) -> &'static str {
        match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Int(IntKind::UInt | IntKind::UInt128) => "u",
            TypeKind::Int(IntKind::Long) => "l",
            TypeKind::Int(IntKind::ULong) => "ul",
            TypeKind::Int(IntKind::LongLong) => "ll",
            TypeKind::Int(IntKind::ULongLong) => "ull",
            _ => "",
        }
    }

    /// What gcc says when the number does not name a parameter of the function of the kind the
    /// attribute wants, and whether it does.
    ///
    /// Zero names nothing, since the parameters are counted from one. A function declared
    /// without a prototype has parameters nobody can count, and any other number is taken.
    /// Otherwise the number has to be one of the parameters, the `...` not being one, and the
    /// parameter has to be what is wanted.
    pub(in crate::check) fn names_a(
        &mut self,
        attr: Attribute,
        function: FunctionId,
        value: i128,
        spelled: &str,
        lead: Lead,
        wanted: Wanted,
    ) -> bool {
        let warn = |checker: &mut Self, what: String| {
            checker.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
            false
        };
        let words = lead.words();
        if value == 0 {
            let what =
                format!("{words} value '{spelled}' does not refer to a function parameter");
            return warn(self, what);
        }
        let signature = self.types.signature(function);
        if !signature.prototyped {
            return true;
        }
        let params = signature.params.clone();
        let Some(&param) = usize::try_from(value).ok().and_then(|n| params.get(n - 1)) else {
            let what = format!(
                "{words} value '{spelled}' exceeds the number of function parameters {}",
                params.len()
            );
            return warn(self, what);
        };
        let kind = self.types.kind(self.types.canonical(param));
        let matches = match wanted {
            Wanted::Pointer => matches!(kind, TypeKind::Pointer(_)),
            Wanted::Integer => {
                rucc_types::is_integer(&self.types, self.types.canonical(param))
                    && !matches!(kind, TypeKind::Bool)
            }
        };
        if !matches {
            let what = format!(
                "{words} value '{spelled}' refers to parameter type {}",
                self.gcc_quoted(param)
            );
            return warn(self, what);
        }
        true
    }
}
