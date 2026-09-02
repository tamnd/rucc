//! The printer for the typed tree, which is what `--emit=tast` writes.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.1.
//!
//! This one does not print C and does not try to. The tree it prints is not source any more:
//! every conversion the language performs is a node of its own, so the shortest useful
//! expression has more nodes than the program has operators, and writing that back as C would
//! print exactly the text that hides what there is to see. What comes out instead is one node
//! per line, indented by depth, with the type spelled out at every expression.
//!
//! The single most useful thing it does is make a conversion visible. When an IR bug turns out
//! to be a sema bug, the question is almost always which conversion is missing or which one is
//! the wrong one, and this is the artifact that answers it without a debugger.
//!
//! # Cross references
//!
//! A tree with jump tables in it is not a tree. A `switch` holds a table of cases whose bodies
//! are statements inside its own body, and a `goto` names a label defined somewhere else
//! entirely. Printing those by recursion would print the same statement twice, so they are
//! printed as references instead, written `#n` after the word that says what kind of thing `n`
//! counts: `case #3` is the fourth entry of the case table, `decl #3` the fourth declaration,
//! `label #3` the fourth label. The numbers are arena indices, which is what makes a dump
//! greppable: the definition and every use of one thing carry the same number.
//!
//! # Using it
//!
//! ```
//! use rucc_base::Interner;
//! use rucc_diag::Span;
//! use rucc_sema::{Category, Const, Conversion, Expr, ExprKind, Printer, Tast};
//! use rucc_types::{IntKind, Types};
//!
//! let types = Types::new();
//! let names = Interner::new();
//! let (char_type, int) = (types.int(IntKind::Char), types.int(IntKind::Int));
//! let mut tast = Tast::new();
//!
//! let c = tast.add_const(Const::Int(97));
//! let c = tast.expr(Expr::new(ExprKind::Const(c), char_type, Category::Rvalue), Span::DUMMY);
//! let widened = ExprKind::Convert { kind: Conversion::Arithmetic, operand: c };
//! let widened = tast.expr(Expr::new(widened, int, Category::Rvalue), Span::DUMMY);
//!
//! let mut printer = Printer::new(&tast, &types, &names);
//! printer.expr(widened);
//! assert_eq!(printer.finish(), "convert arithmetic : int\n  const 97 : char\n");
//! ```

use rucc_ast::AsmQuals;
use rucc_base::Interner;
use rucc_types::{TypeKind, Types, spell};

use crate::asm::{AsmId, AsmOperandList};
use crate::decl::{DeclId, DeclKind, Definition, Linkage, StorageDuration};
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::stmt::{CaseId, Stmt, StmtId};
use crate::tast::{Base, Const, LabelId, Tast};

/// The whole typed translation unit, as text.
#[must_use]
pub fn print(tast: &Tast, types: &Types, names: &Interner) -> String {
    let mut printer = Printer::new(tast, types, names);
    printer.unit();
    printer.finish()
}

/// A typed tree being written out.
///
/// The whole unit is [`print()`]. This is here for the caller that wants one subtree, which is
/// what a test wants and what a diagnostic that quotes a node would want.
#[derive(Debug)]
pub struct Printer<'a> {
    tast: &'a Tast,
    types: &'a Types,
    names: &'a Interner,
    out: String,
    depth: usize,
}

impl<'a> Printer<'a> {
    /// A printer over one tree, whose types are in `types` and whose names are in `names`.
    #[must_use]
    pub fn new(tast: &'a Tast, types: &'a Types, names: &'a Interner) -> Printer<'a> {
        Printer { tast, types, names, out: String::new(), depth: 0 }
    }

    /// The text written so far.
    #[must_use]
    pub fn finish(self) -> String {
        self.out
    }

    /// Every declaration of the translation unit, in the order they were declared.
    pub fn unit(&mut self) {
        for &id in self.tast.top_level() {
            self.decl(id);
        }
    }

    /// One declaration, and its initializer or its body.
    pub fn decl(&mut self, id: DeclId) {
        let node = &self.tast[id];
        let mut head = format!("decl #{}", id.index());
        if let Some(name) = node.name {
            head.push(' ');
            head.push_str(self.names.resolve(name));
        }
        head.push_str(" : ");
        head.push_str(&spell(self.types, self.names, node.ty));
        head.push_str(match node.kind {
            DeclKind::Object => " object",
            DeclKind::Function => " function",
        });
        head.push_str(match node.linkage {
            Linkage::None => "",
            Linkage::Internal => " internal",
            Linkage::External => " external",
        });
        if node.kind == DeclKind::Object {
            head.push_str(match node.duration {
                StorageDuration::Static => " static",
                StorageDuration::Thread => " thread",
                StorageDuration::Automatic => " automatic",
            });
        }
        head.push_str(match node.state {
            Definition::Declared => " declared",
            Definition::Tentative => " tentative",
            Definition::Defined => " defined",
        });
        if node.constant {
            head.push_str(" constexpr");
        }
        if let Some(align) = node.alignment {
            head.push_str(&format!(" alignas {align}"));
        }
        self.line(&head);

        // An initializer that is present and empty is `= {}`, which zero-initializes and is not
        // the same as no initializer at all, so the word is written whether there is anything
        // under it or not.
        if let Some(list) = node.init {
            self.depth += 1;
            self.line("init");
            self.depth += 1;
            // Copied out because printing a value takes `&mut self`, so the borrow of the
            // table cannot be held across the walk. The same is true of every run below.
            let entries = self.tast[list].to_vec();
            for entry in entries {
                let mut at = format!("+{}", entry.offset);
                if entry.is_bit_field() {
                    at.push_str(&format!(" bit {} width {}", entry.bit_offset, entry.bit_width));
                }
                self.line(&at);
                self.depth += 1;
                self.expr(entry.value);
                self.depth -= 1;
            }
            self.depth -= 2;
        }
        // Before the body, because the body refers to them and a reader who meets `decl #1` in
        // an expression should have been told what it is first.
        let params = self.tast[id].params;
        if !params.is_empty() {
            self.depth += 1;
            self.line("params");
            self.depth += 1;
            let params = self.tast[params].to_vec();
            for param in params {
                self.decl(param);
            }
            self.depth -= 2;
        }
        if let Some(body) = self.tast[id].body {
            self.depth += 1;
            self.line("body");
            self.depth += 1;
            self.stmt(body);
            self.depth -= 2;
        }
    }

    /// One statement and everything under it.
    pub fn stmt(&mut self, id: StmtId) {
        match self.tast[id] {
            Stmt::Error => self.line("error"),
            Stmt::Empty => self.line("empty"),
            Stmt::Expr(value) => {
                self.line("expr");
                self.under(|p| p.expr(value));
            }
            Stmt::Block(body) => {
                self.line("block");
                self.depth += 1;
                let body = self.tast[body].to_vec();
                for stmt in body {
                    self.stmt(stmt);
                }
                self.depth -= 1;
            }
            Stmt::Decls(decls) => {
                self.line("decls");
                self.depth += 1;
                let decls = self.tast[decls].to_vec();
                for decl in decls {
                    self.decl(decl);
                }
                self.depth -= 1;
            }
            Stmt::If { cond, then, otherwise } => {
                self.line("if");
                self.depth += 1;
                self.group("cond", |p| p.expr(cond));
                self.group("then", |p| p.stmt(then));
                if let Some(otherwise) = otherwise {
                    self.group("else", |p| p.stmt(otherwise));
                }
                self.depth -= 1;
            }
            Stmt::While { cond, body } => {
                self.line("while");
                self.depth += 1;
                self.group("cond", |p| p.expr(cond));
                self.group("body", |p| p.stmt(body));
                self.depth -= 1;
            }
            Stmt::DoWhile { body, cond } => {
                self.line("do-while");
                self.depth += 1;
                self.group("body", |p| p.stmt(body));
                self.group("cond", |p| p.expr(cond));
                self.depth -= 1;
            }
            Stmt::For { init, cond, step, body } => {
                self.line("for");
                self.depth += 1;
                if let Some(init) = init {
                    self.group("init", |p| p.stmt(init));
                }
                if let Some(cond) = cond {
                    self.group("cond", |p| p.expr(cond));
                }
                if let Some(step) = step {
                    self.group("step", |p| p.expr(step));
                }
                self.group("body", |p| p.stmt(body));
                self.depth -= 1;
            }
            Stmt::Switch { cond, body, cases, default } => {
                self.line("switch");
                self.depth += 1;
                self.group("cond", |p| p.expr(cond));
                self.line("cases");
                self.depth += 1;
                for index in cases.iter() {
                    self.case(index);
                }
                if default.is_some() {
                    self.line("default");
                }
                self.depth -= 1;
                self.group("body", |p| p.stmt(body));
                self.depth -= 1;
            }
            // The value is in the table under the `switch` and is not repeated here, so that
            // the jump table has one home and a case in the body is a reference into it.
            Stmt::Case { case, body } => {
                self.line(&format!("case #{}", case.index()));
                self.under(|p| p.stmt(body));
            }
            Stmt::Default { body } => {
                self.line("default");
                self.under(|p| p.stmt(body));
            }
            Stmt::Label { label, body } => {
                let head = self.label(label);
                self.line(&format!("label {head}"));
                self.under(|p| p.stmt(body));
            }
            Stmt::Goto(label) => {
                let target = self.label(label);
                self.line(&format!("goto {target}"));
            }
            Stmt::IndirectGoto(target) => {
                self.line("indirect-goto");
                self.under(|p| p.expr(target));
            }
            Stmt::Asm(asm) => self.asm(asm),
            Stmt::Break => self.line("break"),
            Stmt::Continue => self.line("continue"),
            Stmt::Return(None) => self.line("return"),
            Stmt::Return(Some(value)) => {
                self.line("return");
                self.under(|p| p.expr(value));
            }
        }
    }

    /// One assembly statement, with its operands in the order the template numbers them.
    ///
    /// The operands are flat rather than grouped under `outputs` and `inputs`, because the
    /// numbering runs through both of them and a reader counting to find `%2` should be able to
    /// count lines. Each one says whether it travels as an address, which is a decision made
    /// here rather than in the walk and is the kind of thing this dump exists to show.
    fn asm(&mut self, id: AsmId) {
        let node = self.tast[id];
        let mut head = String::from("asm");
        for (qual, name) in [
            (AsmQuals::VOLATILE, " volatile"),
            (AsmQuals::INLINE, " inline"),
            (AsmQuals::GOTO, " goto"),
        ] {
            if node.quals.has(qual) {
                head.push_str(name);
            }
        }
        self.line(&head);
        self.depth += 1;
        self.line(&format!("template {}", self.tast[node.template].spell()));
        self.asm_operands(node.outputs, "output");
        self.asm_operands(node.inputs, "input");
        for index in 0..self.tast[node.clobbers].len() {
            let clobber = self.tast[node.clobbers][index];
            self.line(&format!("clobber {}", self.tast[clobber].spell()));
        }
        for index in 0..self.tast[node.labels].len() {
            let label = self.tast[node.labels][index];
            let head = self.label(label);
            self.line(&format!("label {head}"));
        }
        self.depth -= 1;
    }

    /// One section of an assembly statement's operands.
    fn asm_operands(&mut self, list: AsmOperandList, what: &str) {
        for index in 0..self.tast[list].len() {
            let operand = self.tast[list][index];
            let name = match operand.name {
                Some(name) => format!(" [{}]", self.names.resolve(name)),
                None => String::new(),
            };
            let memory = if operand.memory { " memory" } else { "" };
            let constraint = self.tast[operand.constraint].spell();
            self.line(&format!("{what}{name} {constraint}{memory}"));
            self.under(|p| p.expr(operand.value));
        }
    }

    /// One expression, its type, and everything under it.
    pub fn expr(&mut self, id: ExprId) {
        let node = self.tast[id];
        let head = self.head(node);
        let ty = spell(self.types, self.names, node.ty);
        let category = match node.category {
            Category::Rvalue => "",
            Category::Lvalue => " lvalue",
            Category::Bitfield => " bit-field",
            Category::Function => " function",
        };
        self.line(&format!("{head} : {ty}{category}"));
        self.depth += 1;
        self.operands(node.kind);
        self.depth -= 1;
    }

    /// What an expression is, without its type or its operands.
    fn head(&self, node: Expr) -> String {
        match node.kind {
            ExprKind::Error => "error".to_owned(),
            ExprKind::Const(value) => match self.tast[value] {
                // Hexadecimal for the same reason the C printer uses it: a decimal spelling
                // that reads back unchanged needs a shortest round trip algorithm, and one
                // without such an algorithm quietly prints a different number.
                Const::Int(value) => format!("const {value}"),
                Const::Float(value) => format!("const {}", value.to_hex()),
                Const::Address(address) => {
                    let base = match address.base {
                        Base::Decl(decl) => format!("decl #{}", decl.index()),
                        Base::Str(id) => format!("string {}", self.tast[id].spell()),
                    };
                    format!("const address {base} + {}", address.offset)
                }
            },
            ExprKind::Str(value) => format!("string {}", self.tast[value].spell()),
            ExprKind::Decl(decl) => {
                let mut head = format!("decl #{}", decl.index());
                if let Some(name) = self.tast[decl].name {
                    head.push(' ');
                    head.push_str(self.names.resolve(name));
                }
                head
            }
            ExprKind::Member { base, field } => {
                let mut head = format!("member #{field}");
                if let Some(name) = self.field_name(base, field) {
                    head.push(' ');
                    head.push_str(name);
                }
                head
            }
            ExprKind::Subscript { .. } => "subscript".to_owned(),
            ExprKind::Call { .. } => "call".to_owned(),
            ExprKind::Unary { op, .. } if op.is_postfix() => {
                format!("unary post {}", op.spelling())
            }
            ExprKind::Unary { op, .. } => format!("unary {}", op.spelling()),
            ExprKind::Binary { op, .. } => format!("binary {}", op.spelling()),
            // The computation type is written only when it is not the type of the assignment
            // itself, which is the case that is worth seeing: `i /= 0.5` divides in `double`.
            ExprKind::Assign { op, computation, .. } => {
                let mut head = match op {
                    None => "assign =".to_owned(),
                    Some(op) => format!("assign {}=", op.spelling()),
                };
                if computation != node.ty {
                    let ty = spell(self.types, self.names, computation);
                    head.push_str(&format!(" in {ty}"));
                }
                head
            }
            ExprKind::Cond { .. } => "cond".to_owned(),
            ExprKind::Comma { .. } => "comma".to_owned(),
            ExprKind::Cast(_) => "cast".to_owned(),
            ExprKind::Convert { kind, .. } => format!("convert {}", kind.as_str()),
            ExprKind::CompoundLiteral(decl) => format!("compound-literal #{}", decl.index()),
            ExprKind::StmtExpr(_) => "stmt-expr".to_owned(),
            ExprKind::LabelAddr(label) => format!("label-addr {}", self.label(label)),
            ExprKind::VaArg { .. } => "va-arg".to_owned(),
            ExprKind::VaStart { .. } => "va-start".to_owned(),
            ExprKind::VaEnd { .. } => "va-end".to_owned(),
            ExprKind::VaCopy { .. } => "va-copy".to_owned(),
        }
    }

    /// Whatever hangs under an expression, already indented by the caller.
    fn operands(&mut self, kind: ExprKind) {
        match kind {
            ExprKind::Error
            | ExprKind::Const(_)
            | ExprKind::Str(_)
            | ExprKind::Decl(_)
            | ExprKind::LabelAddr(_) => {}
            // A compound literal is a declaration of its own, printed where it is used, since
            // it has no other place in the tree to be printed from.
            ExprKind::CompoundLiteral(decl) => self.decl(decl),
            ExprKind::StmtExpr(body) => self.stmt(body),
            ExprKind::Member { base, .. }
            | ExprKind::Cast(base)
            | ExprKind::VaArg { list: base }
            | ExprKind::VaStart { list: base }
            | ExprKind::VaEnd { list: base }
            | ExprKind::Convert { operand: base, .. }
            | ExprKind::Unary { operand: base, .. } => self.expr(base),
            ExprKind::Subscript { base: lhs, index: rhs }
            | ExprKind::Binary { lhs, rhs, .. }
            | ExprKind::Assign { lhs, rhs, .. }
            | ExprKind::VaCopy { dst: lhs, src: rhs }
            | ExprKind::Comma { lhs, rhs } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Call { callee, args } => {
                self.expr(callee);
                let args = self.tast[args].to_vec();
                for arg in args {
                    self.expr(arg);
                }
            }
            ExprKind::Cond { cond, then, otherwise } => {
                self.expr(cond);
                self.expr(then);
                self.expr(otherwise);
            }
        }
    }

    /// One entry of a case table, which is a value or a range of them.
    fn case(&mut self, id: CaseId) {
        let case = self.tast[id];
        let head = if case.low == case.high {
            format!("case #{} {}", id.index(), case.low)
        } else {
            format!("case #{} {} ... {}", id.index(), case.low, case.high)
        };
        self.line(&head);
    }

    /// A label, as its index and its name.
    fn label(&self, id: LabelId) -> String {
        format!("#{} {}", id.index(), self.names.resolve(self.tast[id].name))
    }

    /// The name of the member at an index, where the base is a record that has one there.
    ///
    /// It is a convenience and not a fact the tree depends on. The index is what the node
    /// holds, an anonymous member has no name to print, and a member of an incomplete record
    /// cannot happen but is not worth panicking over in a printer.
    fn field_name(&self, base: ExprId, field: u32) -> Option<&'a str> {
        let ty = self.types.canonical(self.tast[base].ty);
        let TypeKind::Record(record) = self.types.kind(ty) else { return None };
        let field = self.types.record_info(record).fields.get(field as usize)?;
        Some(self.names.resolve(field.name?))
    }

    /// Writes a named group and puts what the closure writes one level under it.
    fn group(&mut self, name: &str, write: impl FnOnce(&mut Printer<'a>)) {
        self.line(name);
        self.under(write);
    }

    /// Writes what the closure writes one level in.
    fn under(&mut self, write: impl FnOnce(&mut Printer<'a>)) {
        self.depth += 1;
        write(self);
        self.depth -= 1;
    }

    /// Writes one line at the current depth.
    fn line(&mut self, text: &str) {
        for _ in 0..self.depth {
            self.out.push_str("  ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use rucc_ast::{BinaryOp, UnaryOp};
    use rucc_diag::Span;
    use rucc_types::{ArrayLen, IntKind};

    use super::*;
    use crate::decl::{Decl, DeclList, InitEntry};
    use crate::expr::{Conversion, Expr};
    use crate::stmt::Case;
    use crate::tast::Label;

    struct Fixture {
        tast: Tast,
        types: Types,
        names: Interner,
    }

    impl Fixture {
        fn new() -> Fixture {
            Fixture { tast: Tast::new(), types: Types::new(), names: Interner::new() }
        }

        fn int(&self) -> rucc_types::TypeId {
            self.types.int(IntKind::Int)
        }

        /// An rvalue of the given type and kind, which is most of what a test needs.
        fn value(&mut self, kind: ExprKind, ty: rucc_types::TypeId) -> ExprId {
            self.tast.expr(Expr::new(kind, ty, Category::Rvalue), Span::DUMMY)
        }

        fn constant(&mut self, value: i128, ty: rucc_types::TypeId) -> ExprId {
            let id = self.tast.add_const(Const::Int(value));
            self.value(ExprKind::Const(id), ty)
        }

        fn text(&self, write: impl FnOnce(&mut Printer<'_>)) -> String {
            let mut printer = Printer::new(&self.tast, &self.types, &self.names);
            write(&mut printer);
            printer.finish()
        }
    }

    #[test]
    fn an_expression_carries_its_type_on_every_line() {
        let mut f = Fixture::new();
        let int = f.int();
        let left = f.constant(1, int);
        let right = f.constant(2, int);
        let sum = f.value(ExprKind::Binary { op: BinaryOp::Add, lhs: left, rhs: right }, int);

        assert_eq!(f.text(|p| p.expr(sum)), "binary + : int\n  const 1 : int\n  const 2 : int\n");
    }

    #[test]
    fn a_conversion_is_what_the_dump_is_for() {
        let mut f = Fixture::new();
        let (char_type, long) = (f.types.int(IntKind::Char), f.types.int(IntKind::Long));
        let object = f.tast.decl(object_decl(char_type), Span::DUMMY);
        let name = f
            .tast
            .expr(Expr::new(ExprKind::Decl(object), char_type, Category::Lvalue), Span::DUMMY);
        let read =
            f.value(ExprKind::Convert { kind: Conversion::Lvalue, operand: name }, char_type);
        let widened =
            f.value(ExprKind::Convert { kind: Conversion::Arithmetic, operand: read }, long);

        // The two steps that got a `char` to a `long` are each a line, which is the whole
        // reason this printer exists rather than one that writes the C back.
        assert_eq!(
            f.text(|p| p.expr(widened)),
            "convert arithmetic : long\n  convert lvalue : char\n    decl #0 : char lvalue\n"
        );
    }

    #[test]
    fn a_category_is_written_and_an_rvalue_is_the_silent_one() {
        let mut f = Fixture::new();
        let int = f.int();
        let object = f.tast.decl(object_decl(int), Span::DUMMY);
        let name =
            f.tast.expr(Expr::new(ExprKind::Decl(object), int, Category::Lvalue), Span::DUMMY);
        let bits =
            f.tast.expr(Expr::new(ExprKind::Decl(object), int, Category::Bitfield), Span::DUMMY);

        assert_eq!(f.text(|p| p.expr(name)), "decl #0 : int lvalue\n");
        assert_eq!(f.text(|p| p.expr(bits)), "decl #0 : int bit-field\n");
    }

    #[test]
    fn a_postfix_operator_is_not_printed_as_the_prefix_one() {
        let mut f = Fixture::new();
        let int = f.int();
        let one = f.constant(1, int);
        let post = f.value(ExprKind::Unary { op: UnaryOp::PostInc, operand: one }, int);
        let pre = f.value(ExprKind::Unary { op: UnaryOp::PreInc, operand: one }, int);

        assert!(f.text(|p| p.expr(post)).starts_with("unary post ++"));
        assert!(f.text(|p| p.expr(pre)).starts_with("unary ++ :"));
    }

    #[test]
    fn a_compound_assignment_keeps_its_operator() {
        let mut f = Fixture::new();
        let int = f.int();
        let one = f.constant(1, int);
        let plain =
            f.value(ExprKind::Assign { op: None, computation: int, lhs: one, rhs: one }, int);
        let shl =
            ExprKind::Assign { op: Some(BinaryOp::Shl), computation: int, lhs: one, rhs: one };
        let compound = f.value(shl, int);

        assert!(f.text(|p| p.expr(plain)).starts_with("assign = :"));
        assert!(f.text(|p| p.expr(compound)).starts_with("assign <<= :"));
    }

    #[test]
    fn a_case_is_a_reference_into_the_table_and_not_a_second_copy_of_it() {
        let mut f = Fixture::new();
        let int = f.int();
        let cond = f.constant(0, int);
        let empty = f.tast.stmt(Stmt::Empty, Span::DUMMY);
        let cases = f.tast.add_cases(&[
            Case { low: 1, high: 1, body: empty },
            Case { low: 2, high: 9, body: empty },
        ]);
        let first = f.tast.stmt(
            Stmt::Case { case: cases.iter().next().expect("a case"), body: empty },
            Span::DUMMY,
        );
        let fallback = f.tast.stmt(Stmt::Default { body: empty }, Span::DUMMY);
        let body = f.tast.add_stmt_refs(&[first, fallback]);
        let body = f.tast.stmt(Stmt::Block(body), Span::DUMMY);
        let switch =
            f.tast.stmt(Stmt::Switch { cond, body, cases, default: Some(empty) }, Span::DUMMY);

        assert_eq!(
            f.text(|p| p.stmt(switch)),
            "\
switch
  cond
    const 0 : int
  cases
    case #0 1
    case #1 2 ... 9
    default
  body
    block
      case #0
        empty
      default
        empty
"
        );
    }

    #[test]
    fn a_label_and_the_goto_that_reaches_it_carry_the_same_number() {
        let mut f = Fixture::new();
        let name = f.names.intern("done");
        let label = f.tast.add_label(Label { name, stmt: None });
        let empty = f.tast.stmt(Stmt::Empty, Span::DUMMY);
        let target = f.tast.stmt(Stmt::Label { label, body: empty }, Span::DUMMY);
        let jump = f.tast.stmt(Stmt::Goto(label), Span::DUMMY);
        f.tast.define_label(label, target);

        assert_eq!(f.text(|p| p.stmt(target)), "label #0 done\n  empty\n");
        assert_eq!(f.text(|p| p.stmt(jump)), "goto #0 done\n");
    }

    #[test]
    fn a_declaration_says_what_it_is_and_an_empty_initializer_is_still_one() {
        let mut f = Fixture::new();
        let int = f.int();
        let array = f.types.array(int, ArrayLen::Fixed(2));
        let mut decl = object_decl(array);
        decl.name = Some(f.names.intern("a"));
        decl.linkage = Linkage::Internal;
        decl.duration = StorageDuration::Static;
        decl.alignment = Some(16);
        decl.init = Some(f.tast.add_init_entries(&[]));
        let id = f.tast.decl(decl, Span::DUMMY);

        assert_eq!(
            f.text(|p| p.decl(id)),
            "decl #0 a : int[2] object internal static defined alignas 16\n  init\n"
        );
    }

    #[test]
    fn an_initializer_prints_where_each_value_goes() {
        let mut f = Fixture::new();
        let int = f.int();
        let array = f.types.array(int, ArrayLen::Fixed(2));
        let one = f.constant(1, int);
        let entries = f.tast.add_init_entries(&[
            InitEntry::at(0, one),
            InitEntry { offset: 4, value: one, bit_offset: 3, bit_width: 5 },
        ]);
        let mut decl = object_decl(array);
        decl.init = Some(entries);
        let id = f.tast.decl(decl, Span::DUMMY);

        assert_eq!(
            f.text(|p| p.decl(id)),
            "\
decl #0 : int[2] object automatic defined
  init
    +0
      const 1 : int
    +4 bit 3 width 5
      const 1 : int
"
        );
    }

    #[test]
    fn a_unit_is_its_declarations_in_order() {
        let mut f = Fixture::new();
        let int = f.int();
        let first = f.tast.decl(object_decl(int), Span::DUMMY);
        let second = f.tast.decl(object_decl(int), Span::DUMMY);
        f.tast.add_top_level(first);
        f.tast.add_top_level(second);

        assert_eq!(
            print(&f.tast, &f.types, &f.names),
            "decl #0 : int object automatic defined\ndecl #1 : int object automatic defined\n"
        );
    }

    fn object_decl(ty: rucc_types::TypeId) -> Decl {
        Decl {
            name: None,
            ty,
            kind: DeclKind::Object,
            linkage: Linkage::None,
            duration: StorageDuration::Automatic,
            state: Definition::Defined,
            alignment: None,
            constant: false,
            init: None,
            params: DeclList::EMPTY,
            body: None,
        }
    }
}
