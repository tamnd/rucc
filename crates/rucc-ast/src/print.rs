//! The printer, which turns a tree back into C.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.2.
//!
//! What comes out is not the source that went in. Comments are gone, the layout is the
//! printer's own, and a constant is written in the spelling the printer has rather than the one
//! the author had. What is guaranteed is that parsing the output gives the same tree back, and
//! so that printing it a second time gives the same text. That is the property `--emit=ast` is
//! worth having: a printer that agrees with the parser is a check on both of them, and one that
//! merely looks right is a check on nothing.
//!
//! # What that costs
//!
//! Three things are written in a way that reads oddly and round-trips exactly.
//!
//! A floating constant comes out in hexadecimal, so `1.0` prints as `0x1p+0`. A decimal
//! constant that reads back unchanged needs a shortest-round-trip algorithm, and printing one
//! without such an algorithm quietly changes the program. Hexadecimal is exact by construction.
//!
//! A keyword comes out in the spelling that is a keyword in every dialect, so `_Bool` rather
//! than `bool` and `__asm__` rather than `asm`. The tree does not record which dialect it was
//! parsed in, and the ugly spelling is the one that survives all of them.
//!
//! Parentheses come out where the grammar needs them and not where the author wrote them,
//! because the tree does not record them. `(a) + (b)` prints as `a + b`, and `a + b * c` keeps
//! the parentheses it needs and loses the ones it does not.
//!
//! # Using it
//!
//! ```
//! use rucc_ast::{Ast, BinaryOp, Expr, Printer};
//! use rucc_base::Interner;
//! use rucc_diag::Span;
//!
//! let mut interner = Interner::new();
//! let a = interner.intern("a");
//! let mut ast = Ast::new();
//! let left = ast.expr(Expr::Name(a), Span::DUMMY);
//! let right = ast.expr(Expr::Bool(true), Span::DUMMY);
//! let both = ast.expr(Expr::Binary { op: BinaryOp::Add, lhs: left, rhs: right }, Span::DUMMY);
//!
//! let mut printer = Printer::new(&ast, &interner);
//! printer.expr(both);
//! assert_eq!(printer.finish(), "a + true");
//! ```

use rucc_base::{Interner, Symbol};

use crate::asm::{AsmId, AsmQuals};
use crate::ast::{
    AsmOperandList, Ast, AttrList, DesignatorList, EnumeratorList, ExprList, GenericList,
    MemberList, ParamList, StrId, StrList, SymbolList,
};
use crate::attr::{AttrArg, AttrSyntax};
use crate::decl::{
    ArraySize, Decl, DeclId, DeclaratorId, Derived, Field, Member, Param, ParamKind, TypeNameId,
};
use crate::expr::{BinaryOp, Expr, ExprId, UnaryOp};
use crate::init::{Designator, Init, InitId};
use crate::spec::TypeofArg;
use crate::spec::{AlignSpec, Builtin, BuiltinSet, DeclSpecsId, FuncSpecs, Quals, TypeSpec};
use crate::stmt::{ForInit, Stmt, StmtId};

/// The comma operator, which binds least of all.
const COMMA: u8 = 1;
/// Assignment, and the compound assignments.
const ASSIGN: u8 = 2;
/// The conditional operator, which is also what a constant expression is.
const COND: u8 = 3;
/// `||`.
const LOG_OR: u8 = 4;
/// `&&`.
const LOG_AND: u8 = 5;
/// `|`.
const BIT_OR: u8 = 6;
/// `^`.
const BIT_XOR: u8 = 7;
/// `&`.
const BIT_AND: u8 = 8;
/// `==` and `!=`.
const EQUALITY: u8 = 9;
/// `<`, `>`, `<=` and `>=`.
const RELATIONAL: u8 = 10;
/// `<<` and `>>`.
const SHIFT: u8 = 11;
/// `+` and `-`.
const ADDITIVE: u8 = 12;
/// `*`, `/` and `%`.
const MULTIPLICATIVE: u8 = 13;
/// A cast.
const CAST: u8 = 14;
/// The prefix operators.
const UNARY: u8 = 15;
/// The postfix operators, which is also where a compound literal sits.
const POSTFIX: u8 = 16;
/// A name, a constant, and anything that is bracketed all the way round.
const PRIMARY: u8 = 17;

/// How tightly a binary operator binds.
const fn binding(op: BinaryOp) -> u8 {
    match op {
        BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => MULTIPLICATIVE,
        BinaryOp::Add | BinaryOp::Sub => ADDITIVE,
        BinaryOp::Shl | BinaryOp::Shr => SHIFT,
        BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge => RELATIONAL,
        BinaryOp::Eq | BinaryOp::Ne => EQUALITY,
        BinaryOp::BitAnd => BIT_AND,
        BinaryOp::BitXor => BIT_XOR,
        BinaryOp::BitOr => BIT_OR,
        BinaryOp::LogAnd => LOG_AND,
        BinaryOp::LogOr => LOG_OR,
    }
}

/// The type keywords, in the order they are written back out.
///
/// `long` is not here because it is the one that may be written twice, so it is counted rather
/// than held in the set and is put back where it belongs by hand.
const BUILTIN_SPELLINGS: &[(BuiltinSet, &str)] = &[
    (BuiltinSet::SIGNED, "signed"),
    (BuiltinSet::UNSIGNED, "unsigned"),
    (BuiltinSet::SHORT, "short"),
    (BuiltinSet::VOID, "void"),
    (BuiltinSet::BOOL, "_Bool"),
    (BuiltinSet::CHAR, "char"),
    (BuiltinSet::INT, "int"),
    (BuiltinSet::INT128, "__int128"),
    (BuiltinSet::FLOAT, "float"),
    (BuiltinSet::DOUBLE, "double"),
    (BuiltinSet::COMPLEX, "_Complex"),
    (BuiltinSet::IMAGINARY, "_Imaginary"),
    (BuiltinSet::FLOAT16, "_Float16"),
    (BuiltinSet::FLOAT32, "_Float32"),
    (BuiltinSet::FLOAT64, "_Float64"),
    (BuiltinSet::FLOAT128, "_Float128"),
    (BuiltinSet::FLOAT32X, "_Float32x"),
    (BuiltinSet::FLOAT64X, "_Float64x"),
    (BuiltinSet::FLOAT128X, "_Float128x"),
    (BuiltinSet::FLOAT80, "__float80"),
    (BuiltinSet::DECIMAL32, "_Decimal32"),
    (BuiltinSet::DECIMAL64, "_Decimal64"),
    (BuiltinSet::DECIMAL128, "_Decimal128"),
];

/// Whether writing `next` straight after `last` would make one token out of two.
///
/// The check is on the two characters that meet, which is enough: every C token that could be
/// formed by accident starts with a pair that is listed here or is two identifier characters
/// running together. Getting this wrong is how a printer turns `a / *p` into a comment.
fn pastes(last: char, next: char) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    if word(last) && word(next) {
        return true;
    }
    // A pp-number takes a dot on either side of it, which is what makes `case 1 ... 2` need its
    // spaces and `1 .x` need one too.
    if (last == '.' && next.is_ascii_digit()) || (last.is_ascii_digit() && next == '.') {
        return true;
    }
    matches!(
        (last, next),
        ('+', '+' | '=')
            | ('-', '-' | '=' | '>')
            | ('*', '=')
            | ('/', '=' | '/' | '*')
            | ('%', '=' | '>' | ':')
            | ('<', '<' | '=' | ':' | '%')
            | ('>', '>' | '=')
            | ('=', '=')
            | ('!', '=')
            | ('&', '&' | '=')
            | ('|', '|' | '=')
            | ('^', '=')
            | ('.', '.')
            | (':', '>' | ':')
            | ('#', '#')
    )
}

/// Joins two pieces of a declarator, keeping them two tokens if they would otherwise be one.
fn join(mut left: String, right: &str) -> String {
    if let (Some(last), Some(next)) = (left.chars().next_back(), right.chars().next()) {
        if pastes(last, next) {
            left.push(' ');
        }
    }
    left.push_str(right);
    left
}

/// The whole translation unit, as text.
#[must_use]
pub fn print(ast: &Ast, names: &Interner) -> String {
    let mut printer = Printer::new(ast, names);
    printer.unit();
    printer.finish()
}

/// A tree being written out as C.
#[derive(Debug)]
pub struct Printer<'a> {
    ast: &'a Ast,
    names: &'a Interner,
    out: String,
    depth: usize,
}

impl<'a> Printer<'a> {
    /// A printer over one tree, whose names are in `names`.
    #[must_use]
    pub fn new(ast: &'a Ast, names: &'a Interner) -> Printer<'a> {
        Printer { ast, names, out: String::new(), depth: 0 }
    }

    /// The text written so far.
    #[must_use]
    pub fn finish(self) -> String {
        self.out
    }

    /// Every declaration of the translation unit, one after another.
    pub fn unit(&mut self) {
        let ast = self.ast;
        for (index, &decl) in ast.top_level().iter().enumerate() {
            if index > 0 {
                self.newline();
            }
            self.decl(decl);
        }
        if !self.out.is_empty() {
            self.out.push('\n');
        }
    }

    /// One declaration, semicolon included.
    pub fn decl(&mut self, id: DeclId) {
        let ast = self.ast;
        match ast[id] {
            // A poisoned declaration is written as the empty one, which is what it parses back
            // as and which keeps a broken tree printing to a fixed point like any other.
            Decl::Error => self.token(";"),
            Decl::Var { specs, declarators } => {
                self.decl_specs(specs);
                for (index, item) in ast[declarators].iter().enumerate() {
                    if index > 0 {
                        self.token(",");
                    }
                    self.space();
                    let text = self.declarator_text(item.declarator);
                    self.token(&text);
                    if let Some(label) = item.asm_label {
                        self.space();
                        self.token("__asm__");
                        self.token("(");
                        self.string(label);
                        self.token(")");
                    }
                    self.attributes(item.attrs);
                    if let Some(init) = item.init {
                        self.space();
                        self.token("=");
                        self.space();
                        self.init(init);
                    }
                }
                self.token(";");
            }
            Decl::Function { specs, declarator, params, body } => {
                self.decl_specs(specs);
                self.space();
                let text = self.declarator_text(declarator);
                self.token(&text);
                self.depth += 1;
                for &param in &ast[params] {
                    self.newline();
                    self.decl(param);
                }
                self.depth -= 1;
                self.newline();
                self.stmt(body);
            }
            Decl::StaticAssert { cond, message } => {
                self.static_assert(cond, message);
            }
            Decl::Asm(asm) => {
                self.asm(asm);
                self.token(";");
            }
            Decl::Attributes(attrs) => {
                self.attributes(attrs);
                self.token(";");
            }
        }
    }

    /// One statement, on the line it was put on.
    pub fn stmt(&mut self, id: StmtId) {
        let ast = self.ast;
        match ast[id] {
            // Poisoned, and written as the empty statement for the reason a poisoned
            // declaration is written as the empty one.
            Stmt::Error | Stmt::Empty => self.token(";"),
            Stmt::Expr(expr) => {
                self.expr_at(expr, COMMA);
                self.token(";");
            }
            Stmt::Decl(decl) => self.decl(decl),
            Stmt::Compound(items) => {
                self.token("{");
                self.depth += 1;
                for &item in &ast[items] {
                    self.newline();
                    self.stmt(item);
                }
                self.depth -= 1;
                self.newline();
                self.token("}");
            }
            Stmt::If { cond, then, otherwise } => {
                self.token("if");
                self.space();
                self.token("(");
                self.expr_at(cond, COMMA);
                self.token(")");
                if otherwise.is_some() && self.dangling(then) {
                    self.braced(then);
                } else {
                    self.body(then);
                }
                if let Some(otherwise) = otherwise {
                    self.newline();
                    self.token("else");
                    if matches!(ast[otherwise], Stmt::If { .. }) {
                        self.space();
                        self.stmt(otherwise);
                    } else {
                        self.body(otherwise);
                    }
                }
            }
            Stmt::Switch { scrutinee, body } => {
                self.token("switch");
                self.space();
                self.token("(");
                self.expr_at(scrutinee, COMMA);
                self.token(")");
                self.body(body);
            }
            Stmt::While { cond, body } => {
                self.token("while");
                self.space();
                self.token("(");
                self.expr_at(cond, COMMA);
                self.token(")");
                self.body(body);
            }
            Stmt::DoWhile { body, cond } => {
                self.token("do");
                self.body(body);
                self.newline();
                self.token("while");
                self.space();
                self.token("(");
                self.expr_at(cond, COMMA);
                self.token(")");
                self.token(";");
            }
            Stmt::For { init, cond, step, body } => {
                self.token("for");
                self.space();
                self.token("(");
                match init {
                    ForInit::None => self.token(";"),
                    ForInit::Expr(expr) => {
                        self.expr_at(expr, COMMA);
                        self.token(";");
                    }
                    // The declaration writes its own semicolon, since it is a whole declaration
                    // and not an expression that happens to be in a loop header.
                    ForInit::Decl(decl) => self.decl(decl),
                }
                if let Some(cond) = cond {
                    self.space();
                    self.expr_at(cond, COMMA);
                }
                self.token(";");
                if let Some(step) = step {
                    self.space();
                    self.expr_at(step, COMMA);
                }
                self.token(")");
                self.body(body);
            }
            Stmt::Goto(name) => {
                self.token("goto");
                self.space();
                self.name(name);
                self.token(";");
            }
            Stmt::GotoExpr(expr) => {
                self.token("goto");
                self.space();
                self.token("*");
                self.expr_at(expr, CAST);
                self.token(";");
            }
            Stmt::Continue => {
                self.token("continue");
                self.token(";");
            }
            Stmt::Break => {
                self.token("break");
                self.token(";");
            }
            Stmt::Return(value) => {
                self.token("return");
                if let Some(value) = value {
                    self.space();
                    self.expr_at(value, COMMA);
                }
                self.token(";");
            }
            Stmt::Label { name, body, attrs } => {
                self.attributes(attrs);
                self.space();
                self.name(name);
                self.token(":");
                self.labelled(body);
            }
            Stmt::Case { lo, hi, body } => {
                self.token("case");
                self.space();
                self.expr_at(lo, COND);
                if let Some(hi) = hi {
                    self.space();
                    self.token("...");
                    self.space();
                    self.expr_at(hi, COND);
                }
                self.token(":");
                self.labelled(body);
            }
            Stmt::Default { body } => {
                self.token("default");
                self.token(":");
                self.labelled(body);
            }
            Stmt::LocalLabels(names) => {
                self.token("__label__");
                self.name_list(names);
                self.token(";");
            }
            Stmt::Asm(asm) => {
                self.asm(asm);
                self.token(";");
            }
        }
    }

    /// One expression, with no parentheses around it that the grammar does not need.
    pub fn expr(&mut self, id: ExprId) {
        self.expr_at(id, COMMA);
    }

    /// One type name, as it would be written in a cast.
    pub fn type_name(&mut self, id: TypeNameId) {
        let ast = self.ast;
        let name = ast[id];
        self.decl_specs(name.specs);
        let text = self.declarator_text(name.declarator);
        if !text.is_empty() {
            self.space();
            self.token(&text);
        }
    }

    /// The statement a control structure controls, on the same line when it is a block and
    /// indented on the next line when it is not.
    fn body(&mut self, id: StmtId) {
        if matches!(self.ast[id], Stmt::Compound(_)) {
            self.space();
            self.stmt(id);
        } else {
            self.depth += 1;
            self.newline();
            self.stmt(id);
            self.depth -= 1;
        }
    }

    /// A statement in braces it did not have, which is what stops an `else` binding to an `if`
    /// nested inside the branch before it.
    fn braced(&mut self, id: StmtId) {
        self.space();
        self.token("{");
        self.depth += 1;
        self.newline();
        self.stmt(id);
        self.depth -= 1;
        self.newline();
        self.token("}");
    }

    /// The statement a label labels, which C23 allows to be missing at the end of a block.
    fn labelled(&mut self, body: Option<StmtId>) {
        if let Some(body) = body {
            self.newline();
            self.stmt(body);
        }
    }

    /// Whether a statement ends in an `if` with no `else`, and so would take one written after
    /// it.
    fn dangling(&self, id: StmtId) -> bool {
        match self.ast[id] {
            Stmt::If { otherwise: Some(otherwise), .. } => self.dangling(otherwise),
            Stmt::If { otherwise: None, .. } => true,
            Stmt::While { body, .. } | Stmt::Switch { body, .. } | Stmt::For { body, .. } => {
                self.dangling(body)
            }
            Stmt::Label { body: Some(body), .. }
            | Stmt::Case { body: Some(body), .. }
            | Stmt::Default { body: Some(body) } => self.dangling(body),
            _ => false,
        }
    }

    /// `_Static_assert(cond)` or `_Static_assert(cond, "message")`, semicolon included.
    fn static_assert(&mut self, cond: ExprId, message: Option<StrId>) {
        self.token("_Static_assert");
        self.token("(");
        self.expr_at(cond, ASSIGN);
        if let Some(message) = message {
            self.token(",");
            self.space();
            self.string(message);
        }
        self.token(")");
        self.token(";");
    }

    /// An `asm` statement or a file-scope `asm`, without its semicolon.
    fn asm(&mut self, id: AsmId) {
        let ast = self.ast;
        let asm = ast[id];
        self.token("__asm__");
        if asm.quals.has(AsmQuals::VOLATILE) {
            self.token("volatile");
        }
        if asm.quals.has(AsmQuals::INLINE) {
            self.token("inline");
        }
        if asm.quals.has(AsmQuals::GOTO) {
            self.token("goto");
        }
        self.token("(");
        self.string(asm.template);
        // A section is only written when something after it has to be, since the colons are
        // what count the sections and an empty one before a full one cannot be left out.
        let sections = if !asm.labels.is_empty() {
            4
        } else if !asm.clobbers.is_empty() {
            3
        } else if !asm.inputs.is_empty() {
            2
        } else {
            usize::from(!asm.outputs.is_empty())
        };
        for section in 0..sections {
            self.space();
            self.token(":");
            match section {
                0 => self.asm_operands(asm.outputs),
                1 => self.asm_operands(asm.inputs),
                2 => self.string_list(asm.clobbers),
                _ => self.name_list(asm.labels),
            }
        }
        self.token(")");
    }

    /// One section of an `asm` statement's operands.
    fn asm_operands(&mut self, list: AsmOperandList) {
        let ast = self.ast;
        for (index, operand) in ast[list].iter().enumerate() {
            if index > 0 {
                self.token(",");
            }
            self.space();
            if let Some(name) = operand.name {
                self.token("[");
                self.name(name);
                self.token("]");
                self.space();
            }
            self.string(operand.constraint);
            self.space();
            self.token("(");
            self.expr_at(operand.value, COMMA);
            self.token(")");
        }
    }

    /// A comma-separated run of string literals.
    fn string_list(&mut self, list: StrList) {
        let ast = self.ast;
        for (index, &item) in ast[list].iter().enumerate() {
            if index > 0 {
                self.token(",");
            }
            self.space();
            self.string(item);
        }
    }

    /// A comma-separated run of identifiers.
    fn name_list(&mut self, list: SymbolList) {
        let ast = self.ast;
        for (index, &item) in ast[list].iter().enumerate() {
            if index > 0 {
                self.token(",");
            }
            self.space();
            self.name(item);
        }
    }

    /// Everything a declaration says before its first declarator.
    fn decl_specs(&mut self, id: DeclSpecsId) {
        let specs = self.ast[id];
        self.attributes(specs.attrs);
        if let Some(storage) = specs.storage {
            self.token(storage.spelling());
        }
        if specs.thread_local {
            self.token("_Thread_local");
        }
        if specs.constexpr {
            self.token("constexpr");
        }
        if specs.func.has(FuncSpecs::INLINE) {
            self.token("inline");
        }
        if specs.func.has(FuncSpecs::NORETURN) {
            self.token("_Noreturn");
        }
        if let Some(align) = specs.align {
            self.token("_Alignas");
            self.token("(");
            match align {
                AlignSpec::Type(ty) => self.type_name(ty),
                AlignSpec::Expr(expr) => self.expr_at(expr, ASSIGN),
            }
            self.token(")");
        }
        self.quals(specs.quals);
        self.type_spec(specs.ty);
    }

    /// The type qualifiers that were written, in a fixed order.
    fn quals(&mut self, quals: Quals) {
        if quals.has(Quals::CONST) {
            self.token("const");
        }
        if quals.has(Quals::VOLATILE) {
            self.token("volatile");
        }
        if quals.has(Quals::RESTRICT) {
            self.token("restrict");
        }
        if quals.has(Quals::ATOMIC) {
            self.token("_Atomic");
        }
    }

    /// What type a declaration named.
    fn type_spec(&mut self, ty: TypeSpec) {
        match ty {
            TypeSpec::None => {}
            TypeSpec::Builtin(builtin) => self.builtin(builtin),
            // The `#pragma pack` is not written back out. It was not written on the declaration
            // in the first place, it was a line somewhere above it, and there is no attribute
            // that means the same thing, since `pack` caps an alignment where `aligned` raises
            // one. Printing the line here would also put a directive in the middle of whatever
            // the record is nested in, which is not always a place a directive can go.
            TypeSpec::Record { kind, tag, fields, attrs, pack: _ } => {
                self.token(kind.spelling());
                self.attributes(attrs);
                if let Some(tag) = tag {
                    self.space();
                    self.name(tag);
                }
                if let Some(fields) = fields {
                    self.members(fields);
                }
            }
            TypeSpec::Enum { tag, enumerators, underlying, attrs } => {
                self.token("enum");
                self.attributes(attrs);
                if let Some(tag) = tag {
                    self.space();
                    self.name(tag);
                }
                if let Some(underlying) = underlying {
                    self.space();
                    self.token(":");
                    self.space();
                    self.type_name(underlying);
                }
                if let Some(enumerators) = enumerators {
                    self.enumerators(enumerators);
                }
            }
            TypeSpec::Typedef(name) => self.name(name),
            TypeSpec::Typeof { unqual, operand } => {
                self.token(if unqual { "__typeof_unqual__" } else { "__typeof__" });
                self.token("(");
                match operand {
                    TypeofArg::Expr(expr) => self.expr_at(expr, COMMA),
                    TypeofArg::Type(ty) => self.type_name(ty),
                }
                self.token(")");
            }
            TypeSpec::Atomic(ty) => {
                self.token("_Atomic");
                self.token("(");
                self.type_name(ty);
                self.token(")");
            }
            TypeSpec::Auto(which) => self.token(which.spelling()),
            TypeSpec::VaList => self.token("__builtin_va_list"),
        }
    }

    /// The type keywords, in the printer's order rather than the one they were written in.
    fn builtin(&mut self, builtin: Builtin) {
        for &(which, spelling) in BUILTIN_SPELLINGS {
            if builtin.set.has(which) {
                self.token(spelling);
            }
            // `long` goes where it reads, which is after `short` could have been and before
            // everything it can qualify.
            if which == BuiltinSet::SHORT {
                for _ in 0..builtin.longs {
                    self.token("long");
                }
            }
        }
        // `_BitInt` is last because its width follows it, so writing it anywhere else would
        // put a sign between the keyword and the parenthesis it belongs to.
        if let Some(width) = builtin.width {
            self.token("_BitInt");
            self.token("(");
            self.expr_at(width, COMMA);
            self.token(")");
        }
    }

    /// The `{ ... }` of a struct or a union.
    fn members(&mut self, list: MemberList) {
        let ast = self.ast;
        let members = &ast[list];
        self.space();
        self.token("{");
        self.depth += 1;
        let mut index = 0;
        while index < members.len() {
            self.newline();
            match members[index] {
                Member::StaticAssert { cond, message, .. } => {
                    self.static_assert(cond, message);
                    index += 1;
                }
                Member::Field(first) => {
                    self.decl_specs(first.specs);
                    if first.declarator.is_none() && first.bits.is_none() {
                        // An anonymous struct or union member, or a tag declared among the
                        // members. Either way it is a declaration on its own.
                        index += 1;
                    } else {
                        // The members declared together share their specifiers, and they are
                        // written back together so that an anonymous type in them stays one
                        // type rather than becoming one per member.
                        let mut written = 0;
                        while let Some(&Member::Field(field)) = members.get(index) {
                            if field.specs != first.specs
                                || (field.declarator.is_none() && field.bits.is_none())
                            {
                                break;
                            }
                            if written > 0 {
                                self.token(",");
                            }
                            self.space();
                            self.field(field);
                            written += 1;
                            index += 1;
                        }
                    }
                    self.token(";");
                }
            }
        }
        self.depth -= 1;
        self.newline();
        self.token("}");
    }

    /// One member, without the specifiers it shares with the members beside it.
    fn field(&mut self, field: Field) {
        if let Some(declarator) = field.declarator {
            let text = self.declarator_text(declarator);
            self.token(&text);
        }
        if let Some(bits) = field.bits {
            self.space();
            self.token(":");
            self.space();
            self.expr_at(bits, COND);
        }
        self.attributes(field.attrs);
    }

    /// The `{ ... }` of an enumeration, one enumerator to a line.
    fn enumerators(&mut self, list: EnumeratorList) {
        let ast = self.ast;
        self.space();
        self.token("{");
        self.depth += 1;
        for (index, enumerator) in ast[list].iter().enumerate() {
            if index > 0 {
                self.token(",");
            }
            self.newline();
            self.name(enumerator.name);
            self.attributes(enumerator.attrs);
            if let Some(value) = enumerator.value {
                self.space();
                self.token("=");
                self.space();
                self.expr_at(value, COND);
            }
        }
        self.depth -= 1;
        self.newline();
        self.token("}");
    }

    /// A declarator, built from the name outward and given back as its own text.
    ///
    /// Outward is the direction the type reads in and the wrong direction to write in, so the
    /// pieces are assembled here rather than streamed: a pointer step wraps what came before it
    /// on the left, and an array or function step that follows one needs the parentheses that
    /// tell `int (*p)[4]` from `int *p[4]`.
    fn declarator_text(&mut self, id: DeclaratorId) -> String {
        let ast = self.ast;
        let declarator = ast[id];
        let mut text = match declarator.name {
            Some(name) => self.names.resolve(name).to_string(),
            None => String::new(),
        };
        let mut pointered = false;
        for step in &ast[declarator.derived] {
            match *step {
                Derived::Pointer { quals, attrs } => {
                    let prefix = self.capture(|p| {
                        p.token("*");
                        p.quals(quals);
                        p.attributes(attrs);
                    });
                    text = join(prefix, &text);
                    pointered = true;
                }
                Derived::Array { size, quals, has_static } => {
                    if pointered {
                        text = format!("({text})");
                    }
                    let suffix = self.capture(|p| {
                        p.token("[");
                        if has_static {
                            p.token("static");
                        }
                        p.quals(quals);
                        match size {
                            ArraySize::Unspecified => {}
                            ArraySize::Star => p.token("*"),
                            ArraySize::Expr(expr) => p.expr_at(expr, ASSIGN),
                        }
                        p.token("]");
                    });
                    text = join(text, &suffix);
                    pointered = false;
                }
                Derived::Function { params, variadic, kind } => {
                    if pointered {
                        text = format!("({text})");
                    }
                    let suffix = self.capture(|p| p.parameters(params, variadic, kind));
                    text = join(text, &suffix);
                    pointered = false;
                }
            }
        }
        text
    }

    /// A function declarator's parameter list, parentheses included.
    fn parameters(&mut self, params: ParamList, variadic: bool, kind: ParamKind) {
        let ast = self.ast;
        self.token("(");
        match kind {
            ParamKind::Void => self.token("void"),
            ParamKind::Empty => {}
            ParamKind::Identifiers => {
                for (index, param) in ast[params].iter().enumerate() {
                    if index > 0 {
                        self.token(",");
                        self.space();
                    }
                    if let Some(name) = ast[param.declarator].name {
                        self.name(name);
                    }
                }
            }
            ParamKind::Prototype => {
                for (index, param) in ast[params].iter().enumerate() {
                    if index > 0 {
                        self.token(",");
                        self.space();
                    }
                    self.parameter(*param);
                }
                if variadic {
                    if !params.is_empty() {
                        self.token(",");
                        self.space();
                    }
                    self.token("...");
                }
            }
        }
        self.token(")");
    }

    /// One parameter of a prototype.
    fn parameter(&mut self, param: Param) {
        if let Some(specs) = param.specs {
            self.decl_specs(specs);
        }
        let text = self.declarator_text(param.declarator);
        if !text.is_empty() {
            self.space();
            self.token(&text);
        }
        self.attributes(param.attrs);
    }

    /// Every attribute of a list, each in the syntax it was written in.
    fn attributes(&mut self, list: AttrList) {
        let ast = self.ast;
        for attr in &ast[list] {
            self.space();
            match attr.syntax {
                AttrSyntax::Standard => self.token("[["),
                AttrSyntax::Gnu => self.token("__attribute__(("),
                AttrSyntax::Declspec => self.token("__declspec("),
            }
            if let Some(namespace) = attr.namespace {
                self.name(namespace);
                self.token("::");
            }
            self.name(attr.name);
            if !attr.args.is_empty() {
                self.token("(");
                for (index, arg) in ast[attr.args].iter().enumerate() {
                    if index > 0 {
                        self.token(",");
                        self.space();
                    }
                    match *arg {
                        AttrArg::Ident(name) => self.name(name),
                        AttrArg::Expr(expr) => self.expr_at(expr, ASSIGN),
                    }
                }
                self.token(")");
            }
            match attr.syntax {
                AttrSyntax::Standard => self.token("]]"),
                AttrSyntax::Gnu => self.token("))"),
                AttrSyntax::Declspec => self.token(")"),
            }
            // Whatever comes next reads as part of the attribute without this. Nothing needs it
            // to lex, and `token` takes it back where what follows is punctuation.
            self.space();
        }
    }

    /// An initializer, which is an expression or a braced list.
    fn init(&mut self, id: InitId) {
        let ast = self.ast;
        match ast[id] {
            Init::Expr(expr) => self.expr_at(expr, ASSIGN),
            Init::List(items) => {
                self.token("{");
                for (index, item) in ast[items].iter().enumerate() {
                    if index > 0 {
                        self.token(",");
                    }
                    self.space();
                    let designators = &ast[item.designators];
                    for designator in designators {
                        self.designator(*designator);
                    }
                    // The obsolete `name:` form carries its own colon and takes no `=`.
                    let obsolete = matches!(designators.last(), Some(Designator::ObsoleteField(_)));
                    if !designators.is_empty() && !obsolete {
                        self.space();
                        self.token("=");
                        self.space();
                    }
                    self.init(item.init);
                }
                self.space();
                self.token("}");
            }
        }
    }

    /// One step of a designation, or of a `__builtin_offsetof` path.
    fn designator(&mut self, designator: Designator) {
        match designator {
            Designator::Field(name) => {
                self.token(".");
                self.name(name);
            }
            Designator::Index(index) => {
                self.token("[");
                self.expr_at(index, COMMA);
                self.token("]");
            }
            Designator::Range { lo, hi } => {
                self.token("[");
                self.expr_at(lo, COND);
                self.space();
                self.token("...");
                self.space();
                self.expr_at(hi, COND);
                self.token("]");
            }
            Designator::ObsoleteField(name) => {
                self.name(name);
                self.token(":");
                self.space();
            }
        }
    }

    /// An expression, in parentheses when what encloses it binds more tightly than it does.
    fn expr_at(&mut self, id: ExprId, min: u8) {
        if self.precedence(id) < min {
            self.token("(");
            self.expression(id);
            self.token(")");
        } else {
            self.expression(id);
        }
    }

    /// How tightly an expression holds together, which decides whether it needs parentheses.
    fn precedence(&self, id: ExprId) -> u8 {
        match self.ast[id] {
            Expr::Comma { .. } => COMMA,
            Expr::Assign { .. } => ASSIGN,
            Expr::Cond { .. } => COND,
            Expr::Binary { op, .. } => binding(op),
            Expr::Cast { .. } => CAST,
            Expr::Unary { op, .. } => {
                if op.is_postfix() {
                    POSTFIX
                } else {
                    UNARY
                }
            }
            Expr::SizeofExpr(_) | Expr::AlignofExpr(_) | Expr::Extension(_) => UNARY,
            Expr::Index { .. }
            | Expr::Call { .. }
            | Expr::Member { .. }
            | Expr::CompoundLiteral { .. } => POSTFIX,
            _ => PRIMARY,
        }
    }

    /// One expression, with no regard for what encloses it.
    fn expression(&mut self, id: ExprId) {
        let ast = self.ast;
        match ast[id] {
            // Poisoned, and written as a constant so that a broken tree still prints to
            // something that parses.
            Expr::Error => self.token("0"),
            Expr::Name(name) => self.name(name),
            Expr::Int(constant) => {
                let constant = ast[constant];
                let text = format!("{}{}", constant.value, constant.ty.suffix());
                self.token(&text);
            }
            Expr::Float(constant) => {
                let constant = ast[constant];
                let mut text = constant.value.to_hex();
                text.push_str(constant.ty.suffix());
                if constant.imaginary {
                    text.push('i');
                }
                self.token(&text);
            }
            Expr::Char(constant) => {
                let text = ast[constant].spell();
                self.token(&text);
            }
            Expr::Str(literal) => self.string(literal),
            Expr::Bool(value) => self.token(if value { "true" } else { "false" }),
            Expr::Nullptr => self.token("nullptr"),
            Expr::Index { base, index } => {
                self.expr_at(base, POSTFIX);
                self.token("[");
                self.expr_at(index, COMMA);
                self.token("]");
            }
            Expr::Call { callee, args } => {
                self.expr_at(callee, POSTFIX);
                self.token("(");
                self.arguments(args);
                self.token(")");
            }
            Expr::Member { base, name, arrow } => {
                self.expr_at(base, POSTFIX);
                self.token(if arrow { "->" } else { "." });
                self.name(name);
            }
            Expr::Unary { op, operand } => {
                if op.is_postfix() {
                    self.expr_at(operand, POSTFIX);
                    self.token(op.spelling());
                } else {
                    self.token(op.spelling());
                    let inner = match op {
                        UnaryOp::PreInc | UnaryOp::PreDec => UNARY,
                        _ => CAST,
                    };
                    self.expr_at(operand, inner);
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let at = binding(op);
                self.expr_at(lhs, at);
                self.space();
                self.token(op.spelling());
                self.space();
                // The right operand needs one more, since every binary operator in C groups to
                // the left and `a - (b - c)` is not `a - b - c`.
                self.expr_at(rhs, at + 1);
            }
            Expr::Assign { op, lhs, rhs } => {
                self.expr_at(lhs, UNARY);
                self.space();
                match op {
                    Some(op) => {
                        let text = format!("{}=", op.spelling());
                        self.token(&text);
                    }
                    None => self.token("="),
                }
                self.space();
                self.expr_at(rhs, ASSIGN);
            }
            Expr::Cond { cond, then, otherwise } => {
                self.expr_at(cond, COND + 1);
                self.space();
                self.token("?");
                if let Some(then) = then {
                    self.space();
                    self.expr_at(then, COMMA);
                }
                self.space();
                self.token(":");
                self.space();
                self.expr_at(otherwise, COND);
            }
            Expr::Comma { lhs, rhs } => {
                self.expr_at(lhs, COMMA);
                self.token(",");
                self.space();
                self.expr_at(rhs, ASSIGN);
            }
            Expr::Cast { ty, operand } => {
                self.token("(");
                self.type_name(ty);
                self.token(")");
                self.expr_at(operand, CAST);
            }
            Expr::CompoundLiteral { ty, init } => {
                self.token("(");
                self.type_name(ty);
                self.token(")");
                self.init(init);
            }
            // The operand is bracketed unless it is already a name or a constant, because
            // `sizeof (T){ 0 }` reads as a type in parentheses and is not one.
            Expr::SizeofExpr(operand) => {
                self.token("sizeof");
                self.space();
                self.expr_at(operand, PRIMARY);
            }
            Expr::SizeofType(ty) => {
                self.token("sizeof");
                self.token("(");
                self.type_name(ty);
                self.token(")");
            }
            Expr::AlignofExpr(operand) => {
                self.token("__alignof__");
                self.space();
                self.expr_at(operand, PRIMARY);
            }
            Expr::AlignofType(ty) => {
                self.token("_Alignof");
                self.token("(");
                self.type_name(ty);
                self.token(")");
            }
            Expr::Generic { control, assocs } => {
                self.token("_Generic");
                self.token("(");
                self.expr_at(control, ASSIGN);
                self.associations(assocs);
                self.token(")");
            }
            Expr::StmtExpr(body) => {
                self.token("(");
                self.stmt(body);
                self.token(")");
            }
            Expr::LabelAddr(name) => {
                self.token("&&");
                self.name(name);
            }
            Expr::Offsetof { ty, path } => {
                self.token("__builtin_offsetof");
                self.token("(");
                self.type_name(ty);
                self.token(",");
                self.space();
                self.member_path(path);
                self.token(")");
            }
            Expr::ChooseExpr { cond, then, otherwise } => {
                self.token("__builtin_choose_expr");
                self.token("(");
                self.expr_at(cond, ASSIGN);
                self.token(",");
                self.space();
                self.expr_at(then, ASSIGN);
                self.token(",");
                self.space();
                self.expr_at(otherwise, ASSIGN);
                self.token(")");
            }
            Expr::TypesCompatible { a, b } => {
                self.token("__builtin_types_compatible_p");
                self.token("(");
                self.type_name(a);
                self.token(",");
                self.space();
                self.type_name(b);
                self.token(")");
            }
            Expr::VaArg { list, ty } => {
                self.token("__builtin_va_arg");
                self.token("(");
                self.expr_at(list, ASSIGN);
                self.token(",");
                self.space();
                self.type_name(ty);
                self.token(")");
            }
            Expr::VaStart { list, last } => {
                self.token("__builtin_va_start");
                self.token("(");
                self.expr_at(list, ASSIGN);
                if let Some(last) = last {
                    self.token(",");
                    self.space();
                    self.expr_at(last, ASSIGN);
                }
                self.token(")");
            }
            Expr::VaEnd { list } => {
                self.token("__builtin_va_end");
                self.token("(");
                self.expr_at(list, ASSIGN);
                self.token(")");
            }
            Expr::VaCopy { dst, src } => {
                self.token("__builtin_va_copy");
                self.token("(");
                self.expr_at(dst, ASSIGN);
                self.token(",");
                self.space();
                self.expr_at(src, ASSIGN);
                self.token(")");
            }
            Expr::Extension(operand) => {
                self.token("__extension__");
                self.space();
                self.expr_at(operand, CAST);
            }
        }
    }

    /// The arguments of a call, which are assignment-expressions so that the commas between
    /// them stay separators.
    fn arguments(&mut self, args: ExprList) {
        let ast = self.ast;
        for (index, &arg) in ast[args].iter().enumerate() {
            if index > 0 {
                self.token(",");
                self.space();
            }
            self.expr_at(arg, ASSIGN);
        }
    }

    /// The arms of a `_Generic`, the leading comma of each included.
    fn associations(&mut self, assocs: GenericList) {
        let ast = self.ast;
        for assoc in &ast[assocs] {
            self.token(",");
            self.space();
            match assoc.ty {
                Some(ty) => self.type_name(ty),
                None => self.token("default"),
            }
            self.token(":");
            self.space();
            self.expr_at(assoc.value, ASSIGN);
        }
    }

    /// The member path of a `__builtin_offsetof`, whose first step is written with no dot.
    fn member_path(&mut self, path: DesignatorList) {
        let ast = self.ast;
        for (index, step) in ast[path].iter().enumerate() {
            match (index, *step) {
                (0, Designator::Field(name)) => self.name(name),
                (_, step) => self.designator(step),
            }
        }
    }

    /// A string literal, prefix and quotes included.
    fn string(&mut self, id: StrId) {
        let ast = self.ast;
        let text = ast[id].spell();
        self.token(&text);
    }

    /// An identifier.
    fn name(&mut self, symbol: Symbol) {
        let names = self.names;
        self.token(names.resolve(symbol));
    }

    /// Writes with the output redirected into a buffer of its own, and gives the buffer back.
    fn capture(&mut self, write: impl FnOnce(&mut Printer<'a>)) -> String {
        let held = std::mem::take(&mut self.out);
        write(self);
        std::mem::replace(&mut self.out, held)
    }

    /// Appends one token, with a space in front of it if it would otherwise join the one before.
    fn token(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if text.starts_with([';', ',', ')', ']']) {
            self.unspace();
        }
        if let (Some(last), Some(next)) = (self.out.chars().next_back(), text.chars().next()) {
            if pastes(last, next) {
                self.out.push(' ');
            }
        }
        self.out.push_str(text);
    }

    /// Takes back a space that was written for reading, where what follows turns out not to want
    /// one in front of it. An indent is not one of those spaces and stays.
    fn unspace(&mut self) {
        let kept = self.out.trim_end_matches(' ');
        if !kept.ends_with('\n') {
            self.out.truncate(kept.len());
        }
    }

    /// Appends a space, where one is wanted for reading rather than needed for lexing.
    fn space(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with([' ', '\n']) {
            self.out.push(' ');
        }
    }

    /// Ends the line and indents the next one.
    fn newline(&mut self) {
        while self.out.ends_with(' ') {
            self.out.pop();
        }
        self.out.push('\n');
        for _ in 0..self.depth {
            self.out.push_str("    ");
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_diag::Span;
    use rucc_lex::{CharConstant, Encoding, StringLiteral};

    use super::*;
    use crate::decl::Declarator;
    use crate::spec::{DeclSpecs, StorageClass};

    struct Fixture {
        ast: Ast,
        names: Interner,
    }

    impl Fixture {
        fn new() -> Fixture {
            Fixture { ast: Ast::new(), names: Interner::new() }
        }

        fn text(&self, write: impl FnOnce(&mut Printer<'_>)) -> String {
            let mut printer = Printer::new(&self.ast, &self.names);
            write(&mut printer);
            printer.finish()
        }
    }

    #[test]
    fn two_tokens_that_would_join_get_a_space() {
        let mut fixture = Fixture::new();
        let one = fixture.ast.expr(Expr::Bool(true), Span::DUMMY);
        let minus = fixture.ast.expr(Expr::Unary { op: UnaryOp::Minus, operand: one }, Span::DUMMY);
        let twice =
            fixture.ast.expr(Expr::Unary { op: UnaryOp::Minus, operand: minus }, Span::DUMMY);
        assert_eq!(fixture.text(|p| p.expr(twice)), "- -true");
    }

    #[test]
    fn the_variable_argument_family_prints_as_it_was_written() {
        let mut fixture = Fixture::new();
        let ap = fixture.names.intern("ap");
        let copy = fixture.names.intern("copy");
        let n = fixture.names.intern("n");
        let ap = fixture.ast.expr(Expr::Name(ap), Span::DUMMY);
        let copy = fixture.ast.expr(Expr::Name(copy), Span::DUMMY);
        let n = fixture.ast.expr(Expr::Name(n), Span::DUMMY);

        let start = fixture.ast.expr(Expr::VaStart { list: ap, last: Some(n) }, Span::DUMMY);
        assert_eq!(fixture.text(|p| p.expr(start)), "__builtin_va_start(ap, n)");

        // The second argument is missing rather than written as nothing, which is what a
        // program that leaves it out gets and which is reported later rather than here.
        let alone = fixture.ast.expr(Expr::VaStart { list: ap, last: None }, Span::DUMMY);
        assert_eq!(fixture.text(|p| p.expr(alone)), "__builtin_va_start(ap)");

        let copied = fixture.ast.expr(Expr::VaCopy { dst: copy, src: ap }, Span::DUMMY);
        assert_eq!(fixture.text(|p| p.expr(copied)), "__builtin_va_copy(copy, ap)");

        let end = fixture.ast.expr(Expr::VaEnd { list: ap }, Span::DUMMY);
        assert_eq!(fixture.text(|p| p.expr(end)), "__builtin_va_end(ap)");
    }

    #[test]
    fn parentheses_go_where_the_grammar_needs_them_and_nowhere_else() {
        let mut fixture = Fixture::new();
        let a = fixture.names.intern("a");
        let b = fixture.names.intern("b");
        let c = fixture.names.intern("c");
        let a = fixture.ast.expr(Expr::Name(a), Span::DUMMY);
        let b = fixture.ast.expr(Expr::Name(b), Span::DUMMY);
        let c = fixture.ast.expr(Expr::Name(c), Span::DUMMY);

        let sum = fixture.ast.expr(Expr::Binary { op: BinaryOp::Add, lhs: a, rhs: b }, Span::DUMMY);
        let scaled =
            fixture.ast.expr(Expr::Binary { op: BinaryOp::Mul, lhs: sum, rhs: c }, Span::DUMMY);
        assert_eq!(fixture.text(|p| p.expr(scaled)), "(a + b) * c");

        let product =
            fixture.ast.expr(Expr::Binary { op: BinaryOp::Mul, lhs: b, rhs: c }, Span::DUMMY);
        let total =
            fixture.ast.expr(Expr::Binary { op: BinaryOp::Add, lhs: a, rhs: product }, Span::DUMMY);
        assert_eq!(fixture.text(|p| p.expr(total)), "a + b * c");

        // Left grouping, so the right operand of a subtraction keeps its parentheses.
        let inner =
            fixture.ast.expr(Expr::Binary { op: BinaryOp::Sub, lhs: b, rhs: c }, Span::DUMMY);
        let outer =
            fixture.ast.expr(Expr::Binary { op: BinaryOp::Sub, lhs: a, rhs: inner }, Span::DUMMY);
        assert_eq!(fixture.text(|p| p.expr(outer)), "a - (b - c)");
    }

    #[test]
    fn a_declarator_reads_outward_from_its_name() {
        let mut fixture = Fixture::new();
        let f = fixture.names.intern("f");
        let three = fixture.ast.expr(Expr::Bool(true), Span::DUMMY);
        let derived = fixture.ast.add_derived_list(&[
            Derived::Array { size: ArraySize::Expr(three), quals: Quals::NONE, has_static: false },
            Derived::Pointer { quals: Quals::NONE, attrs: AttrList::EMPTY },
            Derived::Function { params: ParamList::EMPTY, variadic: false, kind: ParamKind::Void },
        ]);
        let declarator = fixture.ast.add_declarator(Declarator {
            name: Some(f),
            name_span: Span::DUMMY,
            derived,
            span: Span::DUMMY,
        });
        let specs = fixture.ast.add_specs(DeclSpecs::empty(Span::DUMMY));
        let ty = fixture.ast.add_type_name(crate::decl::TypeName {
            specs,
            declarator,
            span: Span::DUMMY,
        });
        assert_eq!(fixture.text(|p| p.type_name(ty)), "(*f[true])(void)");
    }

    #[test]
    fn a_declaration_keeps_its_declarators_together() {
        let mut fixture = Fixture::new();
        let a = fixture.names.intern("a");
        let b = fixture.names.intern("b");
        let mut specs = DeclSpecs::empty(Span::DUMMY);
        specs.storage = Some(StorageClass::Static);
        specs.ty = TypeSpec::Builtin(Builtin { set: BuiltinSet::INT, longs: 0, width: None });
        let specs = fixture.ast.add_specs(specs);
        let mut declarators = Vec::new();
        for (name, stars) in [(a, 0), (b, 1)] {
            let derived = if stars == 0 {
                crate::ast::DerivedList::EMPTY
            } else {
                fixture.ast.add_derived_list(&[Derived::Pointer {
                    quals: Quals::NONE,
                    attrs: AttrList::EMPTY,
                }])
            };
            let declarator = fixture.ast.add_declarator(Declarator {
                name: Some(name),
                name_span: Span::DUMMY,
                derived,
                span: Span::DUMMY,
            });
            declarators.push(crate::decl::InitDeclarator {
                declarator,
                init: None,
                asm_label: None,
                attrs: AttrList::EMPTY,
                span: Span::DUMMY,
            });
        }
        let declarators = fixture.ast.add_init_declarator_list(&declarators);
        let decl = fixture.ast.decl(Decl::Var { specs, declarators }, Span::DUMMY);
        assert_eq!(fixture.text(|p| p.decl(decl)), "static int a, *b;");
    }

    #[test]
    fn a_byte_escape_in_a_string_takes_three_octal_digits() {
        let literal = StringLiteral {
            elements: vec![0xff, u32::from(b'0'), u32::from(b'a')],
            encoding: Encoding::Plain,
            remarks: rucc_lex::Remarks::NONE,
        };
        assert_eq!(literal.spell(), "\"\\3770a\"");
    }

    #[test]
    fn a_wide_escape_closes_the_literal_rather_than_swallowing_what_follows() {
        let literal = StringLiteral {
            elements: vec![0x1234, u32::from(b'a'), u32::from(b'z')],
            encoding: Encoding::Utf32,
            remarks: rucc_lex::Remarks::NONE,
        };
        assert_eq!(literal.spell(), "U\"\\x1234\" U\"az\"");
    }

    #[test]
    fn a_character_constant_is_written_as_a_character_where_it_can_be() {
        let plain = CharConstant {
            value: i64::from(b'a'),
            encoding: Encoding::Plain,
            remarks: rucc_lex::Remarks::NONE,
        };
        assert_eq!(plain.spell(), "'a'");

        let quote = CharConstant { encoding: Encoding::Plain, value: i64::from(b'\''), ..plain };
        assert_eq!(quote.spell(), "'\\''");

        let negative = CharConstant { value: -1, ..plain };
        assert_eq!(negative.spell(), "'\\xff'");

        let many = CharConstant { value: 0x6162, ..plain };
        assert_eq!(many.spell(), "'\\x61\\x62'");
    }
}
