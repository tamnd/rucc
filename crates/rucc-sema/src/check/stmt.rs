//! Statements: what happens, in what order, and where control is allowed to go instead.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.12.
//!
//! An expression is checked against the types of its operands and nothing else, which is why the
//! expression checking is a walk with no state in it. A statement is not. Whether `break` is
//! allowed depends on what encloses it, what `return` may carry depends on the function it is in,
//! and a `goto` may name a label that is fifty lines further down. So this walk carries a [`Body`]
//! for as long as it is inside one, and everything a statement needs to know that is not in the
//! statement itself is in there.
//!
//! # Labels are resolved over the whole function and not in order
//!
//! A label is one namespace, scoped to the function, and a `goto` is allowed to come first. So a
//! label is created where its name is first met, whether that is the `goto` or the label itself,
//! and the statement it names is filled in later. What is left over at the end of the function is
//! the labels that were used and never defined, which is the one diagnostic here that cannot be
//! written where it is found.
//!
//! GNU's `__label__` is the exception: it declares a label local to the block, which is what lets
//! a macro that jumps to its own end be expanded twice in one function without the two colliding.
//! Those are undone when the block ends, which is what the saved bindings in the body are for.
//!
//! # Why the case table is patched
//!
//! A `switch` holds its cases as a run, so that the walk to the IR builds a jump table from a
//! table rather than by searching the body for labels. The run is not known until the body has
//! been walked, and the `case` statements in the body are built while it is being walked, so each
//! of them is written with a placeholder and given its real entry once the run exists. Collecting
//! the whole run at the end is also what keeps a nested `switch` from interleaving its cases with
//! the ones outside it, since each `switch` adds its cases in one go.
//!
//! # What is not here
//!
//! Reachability. `control reaches end of non-void function` and the unreachable code warnings are
//! questions about a control flow graph, and the answer to them is in the IR rather than in the
//! tree, so they wait for it. A label that is defined and never used is a warning gcc only gives
//! under `-Wall`, and it waits for the flag rather than for anything here.

use std::collections::{HashMap, HashSet};
use std::mem;

use rucc_ast::{self as ast, ForInit, StorageClass};
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{IntegerInfo, TypeId, is_integer, is_pointer, is_void};

use crate::check::Checker;
use crate::check::expr::Target;
use crate::eval;
use crate::expr::{Category, Expr, ExprId, ExprKind};
use crate::stmt::{Case, Stmt, StmtId};
use crate::tast::{Const, Label, LabelId};

/// What the statements of one function body are checked against.
#[derive(Debug)]
pub(in crate::check) struct Body {
    /// The return type, which every `return` in it answers to.
    ret: TypeId,
    /// Where the function was named, for the `declared here` note under a `return` that
    /// disagrees with the return type.
    at: Span,
    /// The labels of the function, by the name they were written with.
    labels: HashMap<Symbol, Labelled>,
    /// What the enclosing blocks bound the names of their `__label__` declarations to, so that a
    /// block-local label can be undone when the block ends.
    shadowed: Vec<(Symbol, Option<Labelled>)>,
    /// Where each enclosing block's run of those starts.
    blocks: Vec<usize>,
    /// The `switch` statements this one is inside, innermost last.
    switches: Vec<Switch>,
    /// How many loops it is inside, which is what `continue` asks and half of what `break` asks.
    loops: usize,
    /// The names this function has already been told about, so that a name nobody declared is
    /// reported once rather than once per use. The message says `first use in this function`
    /// and gcc means it: a typo in a loop body is one mistake however many times it is written.
    undeclared: HashSet<Symbol>,
}

/// One label of a function.
#[derive(Debug, Clone, Copy)]
struct Labelled {
    /// The label in the typed tree, made where the name was first met.
    id: LabelId,
    /// Whether the statement it names has been seen, and where the label was written.
    defined: Option<Span>,
    /// Where the name was first met, which is what an undefined label is reported at.
    at: Span,
}

/// One `switch` being checked, and the case table it is collecting.
#[derive(Debug)]
struct Switch {
    /// The promoted type of the controlling expression, which every case value is held in.
    ty: TypeId,
    /// The shape of the type before that promotion, which is the range a case value is warned
    /// about for leaving. gcc measures against what was written rather than against what the
    /// promotion widened it to, so `case 300` on a `char` is worth saying even though 300 is a
    /// perfectly good `int`.
    range: Option<IntegerInfo>,
    /// The cases so far, in the order they were written.
    cases: Vec<Case>,
    /// Where each of them was written, for the note under a duplicate.
    spans: Vec<Span>,
    /// The statements those cases label, which are patched with their table entries once the
    /// table exists.
    labels: Vec<StmtId>,
    /// The `default`, and where it was written, once one has been seen.
    default: Option<(StmtId, Span)>,
}

impl Checker<'_> {
    /// Checks one statement, as though it were the body of a function returning `ret`.
    ///
    /// The entry for a caller that has a statement rather than a translation unit, which is what
    /// the tests here are built on. A body is opened around it and closed after, so that the
    /// labels are resolved and reported the way they are in a real function.
    pub fn check_stmt(&mut self, ret: TypeId, id: ast::StmtId) -> StmtId {
        let previous = self.open_body(ret, Span::DUMMY);
        let stmt = self.stmt(id);
        self.close_body(previous);
        stmt
    }

    /// Checks one statement and gives back the node it became.
    pub(in crate::check) fn stmt(&mut self, id: ast::StmtId) -> StmtId {
        let span = self.ast.stmt_span(id);
        let node = match self.ast[id] {
            ast::Stmt::Error => Stmt::Error,
            ast::Stmt::Empty => Stmt::Empty,
            ast::Stmt::Expr(value) => {
                let value = self.expr(value);
                Stmt::Expr(self.value(value))
            }
            ast::Stmt::Decl(decl) => Stmt::Decls(self.check_decl(decl)),
            ast::Stmt::Compound(body) => Stmt::Block(self.block(body)),
            ast::Stmt::If { cond, then, otherwise } => {
                let cond = self.controlling(cond);
                let then = self.stmt(then);
                Stmt::If { cond, then, otherwise: otherwise.map(|id| self.stmt(id)) }
            }
            ast::Stmt::Switch { scrutinee, body } => self.switch(scrutinee, body),
            ast::Stmt::While { cond, body } => {
                let cond = self.controlling(cond);
                Stmt::While { cond, body: self.loop_body(body) }
            }
            ast::Stmt::DoWhile { body, cond } => {
                let body = self.loop_body(body);
                Stmt::DoWhile { body, cond: self.controlling(cond) }
            }
            ast::Stmt::For { init, cond, step, body } => self.for_loop(init, cond, step, body),
            ast::Stmt::Goto(name) => Stmt::Goto(self.label(name, span)),
            ast::Stmt::GotoExpr(target) => self.computed_goto(target),
            ast::Stmt::Continue => self.continue_stmt(span),
            ast::Stmt::Break => self.break_stmt(span),
            ast::Stmt::Return(value) => self.return_stmt(value, span),
            ast::Stmt::Label { name, body, .. } => self.labelled(name, body, span),
            ast::Stmt::Case { lo, hi, body } => self.case(lo, hi, body, span),
            ast::Stmt::Default { body } => self.default(body, span),
            ast::Stmt::LocalLabels(names) => {
                self.local_labels(names, span);
                Stmt::Empty
            }
            ast::Stmt::Asm(_) => {
                self.statement_unsupported("an assembler statement", span);
                Stmt::Error
            }
        };
        let stmt = self.tast.stmt(node, span);
        // The `switch` patches its cases once it has a table, and what it has to patch is the
        // node that ended up in the body rather than the one the arm above built, so the case
        // is registered here where that node exists.
        if matches!(node, Stmt::Case { .. }) {
            if let Some(switch) = self.switches() {
                switch.labels.push(stmt);
            }
        }
        stmt
    }

    /// `({ ... })`, GNU's statement expression, whose value is its last statement's.
    ///
    /// The type is the last statement's if that statement is an expression, and `void` otherwise,
    /// which is gcc's rule and which makes `({ })` and `({ int x; })` both `void`. This works
    /// because an expression statement holds the value of its expression rather than a conversion
    /// of it to `void`: the statement is what discards the value, and here is where the value is
    /// wanted instead.
    pub(in crate::check) fn stmt_expr(&mut self, id: ast::StmtId, span: Span) -> ExprId {
        let stmt = self.stmt(id);
        let ty = match self.tast[stmt] {
            Stmt::Block(body) => match self.tast[body].last() {
                Some(&last) => match self.tast[last] {
                    Stmt::Expr(value) => self.tast[value].ty,
                    _ => self.types.void(),
                },
                None => self.types.void(),
            },
            _ => self.types.void(),
        };
        self.tast.expr(Expr::new(ExprKind::StmtExpr(stmt), ty, Category::Rvalue), span)
    }

    /// `&&name`, GNU's label address, whose type is `void *` and whose target is a label.
    ///
    /// Mentioning a label here is a use of it and not a definition, so a function that takes the
    /// address of a label it never defines is reported the same way a `goto` to one is.
    pub(in crate::check) fn label_addr(&mut self, name: Symbol, span: Span) -> ExprId {
        let label = self.label(name, span);
        let ty = self.types.pointer(self.types.void());
        self.tast.expr(Expr::new(ExprKind::LabelAddr(label), ty, Category::Rvalue), span)
    }

    /// Opens a body, and gives back the one it displaced so that it can be put back.
    ///
    /// Displaced rather than asserted absent, because GNU's nested functions are a body inside a
    /// body and each has its own labels, its own return type and its own loops.
    pub(in crate::check) fn open_body(&mut self, ret: TypeId, at: Span) -> Option<Body> {
        let body = Body {
            ret,
            at,
            labels: HashMap::new(),
            shadowed: Vec::new(),
            blocks: Vec::new(),
            switches: Vec::new(),
            loops: 0,
            undeclared: HashSet::new(),
        };
        self.body.replace(body)
    }

    /// Whether this is the first time the function being checked has used the undeclared name
    /// `name`, and records it either way.
    ///
    /// Always true outside a function body, where there is nothing to remember it in and where
    /// each declaration is its own context anyway.
    pub(in crate::check) fn first_undeclared_use(&mut self, name: Symbol) -> bool {
        match &mut self.body {
            Some(body) => body.undeclared.insert(name),
            None => true,
        }
    }

    /// Closes a body, reporting the labels that were used and never defined.
    pub(in crate::check) fn close_body(&mut self, previous: Option<Body>) {
        let Some(body) = mem::replace(&mut self.body, previous) else {
            return;
        };
        // Sorted, because a map has no order and a compiler whose diagnostics come out in a
        // different order on two runs of the same input is one nobody can write a test against.
        let mut undefined: Vec<Labelled> =
            body.labels.into_values().filter(|label| label.defined.is_none()).collect();
        undefined.sort_by_key(|label| label.at.lo);
        for label in undefined {
            self.undefined_label(label);
        }
    }

    /// The body of a function definition, walked in the scope its parameters are already in.
    ///
    /// A function body is one scope with the parameters, which is why this exists rather than
    /// the caller reaching [`Checker::stmt`]: that would open a second scope and make
    /// `void f(int a) { int a; }` two declarations of `a` that never meet.
    pub(in crate::check) fn body_block(&mut self, body: ast::StmtId) -> StmtId {
        let span = self.ast.stmt_span(body);
        let ast::Stmt::Compound(list) = self.ast[body] else {
            return self.stmt(body);
        };
        let list = self.statements(list);
        self.tast.stmt(Stmt::Block(list), span)
    }

    /// A block, which is a scope.
    fn block(&mut self, body: ast::StmtList) -> crate::stmt::StmtList {
        self.scopes.push();
        let list = self.statements(body);
        self.scopes.pop();
        list
    }

    /// The statements of a block, with the block-local labels undone at the end of it.
    fn statements(&mut self, body: ast::StmtList) -> crate::stmt::StmtList {
        if let Some(state) = self.body.as_mut() {
            let mark = state.shadowed.len();
            state.blocks.push(mark);
        }
        let ids = self.ast[body].to_vec();
        let mut stmts = Vec::with_capacity(ids.len());
        for id in ids {
            stmts.push(self.stmt(id));
        }
        self.end_block();
        self.tast.add_stmt_refs(&stmts)
    }

    /// Undoes what `__label__` declared in the block that is ending.
    fn end_block(&mut self) {
        let Some(body) = self.body.as_mut() else {
            return;
        };
        let Some(mark) = body.blocks.pop() else {
            return;
        };
        let mut gone = Vec::new();
        while body.shadowed.len() > mark {
            let (name, previous) = body.shadowed.pop().expect("a saved binding");
            let local = match previous {
                Some(previous) => body.labels.insert(name, previous),
                None => body.labels.remove(&name),
            };
            if let Some(local) = local {
                if local.defined.is_none() {
                    gone.push(local);
                }
            }
        }
        gone.sort_by_key(|label| label.at.lo);
        for label in gone {
            self.undefined_label(label);
        }
    }

    /// The body of a loop, inside which `break` and `continue` both mean something.
    fn loop_body(&mut self, body: ast::StmtId) -> StmtId {
        if let Some(state) = self.body.as_mut() {
            state.loops += 1;
        }
        let body = self.stmt(body);
        if let Some(state) = self.body.as_mut() {
            state.loops -= 1;
        }
        body
    }

    /// `for (init; cond; step) body`, whose first clause is in a scope of its own.
    fn for_loop(
        &mut self,
        init: ForInit,
        cond: Option<ast::ExprId>,
        step: Option<ast::ExprId>,
        body: ast::StmtId,
    ) -> Stmt {
        // The scope is the loop's rather than the body's, which is what makes the `i` in
        // `for (int i = 0; ...)` visible to the condition and gone after the loop.
        self.scopes.push();
        let init = match init {
            ForInit::None => None,
            ForInit::Expr(value) => {
                let span = self.ast.expr_span(value);
                let value = self.expr(value);
                let value = self.value(value);
                Some(self.tast.stmt(Stmt::Expr(value), span))
            }
            ForInit::Decl(decl) => {
                let span = self.ast.decl_span(decl);
                let decls = self.check_decl(decl);
                self.check_loop_declaration(decl);
                Some(self.tast.stmt(Stmt::Decls(decls), span))
            }
        };
        let cond = cond.map(|cond| self.controlling(cond));
        let step = step.map(|step| {
            let step = self.expr(step);
            self.value(step)
        });
        let body = self.loop_body(body);
        self.scopes.pop();
        Stmt::For { init, cond, step, body }
    }

    /// What a `for` loop's first clause is not allowed to declare.
    ///
    /// C99 6.8.5p3 says the declaration there declares objects with automatic storage and nothing
    /// else, which rules out a `static`, an `extern` and a `typedef`. The point of the rule is
    /// that the clause scopes to the loop, and a name that outlives the loop has no business
    /// being written where it looks like it does not.
    ///
    /// gcc accepts all three without a word unless `-pedantic` is on, and enough code declares a
    /// `static` counter there that following the letter of the rule by default would reject
    /// programs everyone else builds.
    fn check_loop_declaration(&mut self, decl: ast::DeclId) {
        if !self.cx.pedantic {
            return;
        }
        let ast::Decl::Var { specs, declarators } = self.ast[decl] else {
            return;
        };
        let specs = self.ast[specs];
        let word = match specs.storage {
            _ if specs.is_typedef() => "non-variable",
            Some(StorageClass::Static) => "static variable",
            Some(StorageClass::Extern) => "'extern' variable",
            _ => return,
        };
        let ast = self.ast;
        for &item in &ast[declarators] {
            let node = ast[item.declarator];
            let Some(name) = node.name else { continue };
            let spelled = self.text(name).to_owned();
            self.report(
                Diagnostic::warning(
                    format!("declaration of {word} '{spelled}' in 'for' loop initial declaration"),
                    node.name_span,
                )
                .with_code("E0619"),
            );
        }
    }

    /// `switch (cond) body`, with the case table collected while the body is walked.
    fn switch(&mut self, scrutinee: ast::ExprId, body: ast::StmtId) -> Stmt {
        let at = self.ast.expr_span(scrutinee);
        let cond = self.expr(scrutinee);
        let cond = self.value(cond);
        // Read before the promotion and not after it, because the range a case value is measured
        // against is the one that was written. `switch (c)` on a `char` and `case 300` is worth
        // saying, and by the time the promotion has run there is nothing left to say it about.
        let range = eval::int_shape(&self.types, self.tast[cond].ty, self.cx.target);
        let cond = self.conv().promote(cond);
        let ty = self.tast[cond].ty;
        let cond = if self.is_poisoned(cond) || is_integer(&self.types, ty) {
            cond
        } else {
            self.report(Diagnostic::error("switch quantity not an integer", at).with_code("E0620"));
            self.poison(at)
        };
        // The controlling type is the promoted one even where it was not an integer, so that the
        // cases in the body are still folded and checked against each other rather than being
        // reported a second time for something the `switch` itself already answered for.
        let ty = if is_integer(&self.types, ty) { ty } else { self.int() };
        if let Some(state) = self.body.as_mut() {
            state.switches.push(Switch {
                ty,
                range,
                cases: Vec::new(),
                spans: Vec::new(),
                labels: Vec::new(),
                default: None,
            });
        }
        let body = self.stmt(body);
        let Some(switch) = self.body.as_mut().and_then(|state| state.switches.pop()) else {
            return Stmt::Error;
        };
        let cases = self.tast.add_cases(&switch.cases);
        for (&labelled, case) in switch.labels.iter().zip(cases.iter()) {
            let Stmt::Case { body, .. } = self.tast[labelled] else {
                continue;
            };
            self.tast.set_stmt(labelled, Stmt::Case { case, body });
        }
        Stmt::Switch { cond, body, cases, default: switch.default.map(|(stmt, _)| stmt) }
    }

    /// `case lo:`, or GNU's `case lo ... hi:`.
    fn case(
        &mut self,
        lo: ast::ExprId,
        hi: Option<ast::ExprId>,
        body: Option<ast::StmtId>,
        span: Span,
    ) -> Stmt {
        let body = self.labelled_body(body, span);
        if self.body.as_ref().is_none_or(|state| state.switches.is_empty()) {
            self.report(
                Diagnostic::error("case label not within a switch statement", span)
                    .with_code("E0621"),
            );
            return Stmt::Error;
        }
        let Some(low) = self.case_value(lo, span) else {
            return Stmt::Error;
        };
        let high = match hi {
            Some(hi) => match self.case_value(hi, span) {
                Some(high) => high,
                None => return Stmt::Error,
            },
            None => low,
        };
        if high < low {
            self.report(Diagnostic::warning("empty range specified", span).with_code("E0622"));
            return Stmt::Error;
        }
        if let Some(at) = self.overlapping_case(low, high) {
            self.report(
                Diagnostic::error("duplicate case value", span)
                    .with_code("E0623")
                    .note("previously used here".to_owned(), at),
            );
            return Stmt::Error;
        }
        let switch = self.switches().expect("a switch");
        switch.cases.push(Case { low, high, body });
        switch.spans.push(span);
        // The entry is a placeholder until the `switch` knows where its table went, which is
        // what the walk over its body ends with. The node this becomes is registered by
        // [`Checker::stmt`], since that is where it is written into the arena and only the node
        // that ends up in the body is worth patching.
        let placeholder = rucc_base::Idx::from_usize(0);
        Stmt::Case { case: placeholder, body }
    }

    /// The value of one case label, folded and converted to the controlling type.
    fn case_value(&mut self, value: ast::ExprId, span: Span) -> Option<i128> {
        let at = self.ast.expr_span(value);
        let value = self.expr(value);
        let value = self.value(value);
        let folded = match self.eval_integer(value) {
            Ok(folded) => folded,
            Err(failed) => {
                if !failed.poisoned {
                    self.report(
                        Diagnostic::error("case label does not reduce to an integer constant", at)
                            .with_code("E0624"),
                    );
                }
                return None;
            }
        };
        let switch = self.switches()?;
        let (ty, range) = (switch.ty, switch.range);
        if let Some(range) = range {
            if eval::overflows(Const::Int(folded), range) {
                self.report(
                    Diagnostic::warning("case label value exceeds maximum value for type", span)
                        .with_code("E0625"),
                );
            }
        }
        let info = eval::int_shape(&self.types, ty, self.cx.target)?;
        Some(eval::narrowed(Const::Int(folded), info))
    }

    /// Where a case that already covers part of this range was written, if there is one.
    fn overlapping_case(&mut self, low: i128, high: i128) -> Option<Span> {
        let switch = self.switches()?;
        switch
            .cases
            .iter()
            .position(|case| case.low <= high && low <= case.high)
            .map(|index| switch.spans[index])
    }

    /// `default:`.
    fn default(&mut self, body: Option<ast::StmtId>, span: Span) -> Stmt {
        let body = self.labelled_body(body, span);
        if self.body.as_ref().is_none_or(|state| state.switches.is_empty()) {
            self.report(
                Diagnostic::error("'default' label not within a switch statement", span)
                    .with_code("E0626"),
            );
            return Stmt::Error;
        }
        if let Some((_, at)) = self.switches().expect("a switch").default {
            self.report(
                Diagnostic::error("multiple default labels in one switch", span)
                    .with_code("E0627")
                    .note("this is the first default label".to_owned(), at),
            );
            return Stmt::Error;
        }
        self.switches().expect("a switch").default = Some((body, span));
        Stmt::Default { body }
    }

    /// `name: body`, which defines a label.
    fn labelled(&mut self, name: Symbol, body: Option<ast::StmtId>, span: Span) -> Stmt {
        let body = self.labelled_body(body, span);
        let label = self.label(name, span);
        let defined = self.body.as_ref().and_then(|state| state.labels[&name].defined);
        if let Some(at) = defined {
            let spelled = self.text(name).to_owned();
            self.report(
                Diagnostic::error(format!("duplicate label '{spelled}'"), span)
                    .with_code("E0628")
                    .note(format!("previous definition of '{spelled}' with type 'void'"), at),
            );
            return Stmt::Error;
        }
        if let Some(state) = self.body.as_mut() {
            state.labels.entry(name).and_modify(|known| known.defined = Some(span));
        }
        self.tast.define_label(label, body);
        Stmt::Label { label, body }
    }

    /// The statement a label labels, which C23 allows to be absent at the end of a block.
    fn labelled_body(&mut self, body: Option<ast::StmtId>, span: Span) -> StmtId {
        match body {
            Some(body) => self.stmt(body),
            None => self.tast.stmt(Stmt::Empty, span),
        }
    }

    /// `__label__ a, b;`, which declares labels local to the block it is written in.
    fn local_labels(&mut self, names: ast::SymbolList, span: Span) {
        let ast = self.ast;
        for &name in &ast[names] {
            let id = self.tast.add_label(Label { name, stmt: None });
            let local = Labelled { id, defined: None, at: span };
            if let Some(state) = self.body.as_mut() {
                let previous = state.labels.insert(name, local);
                state.shadowed.push((name, previous));
            }
        }
    }

    /// The label of a name, made where the name is first met.
    fn label(&mut self, name: Symbol, span: Span) -> LabelId {
        if let Some(known) = self.body.as_ref().and_then(|state| state.labels.get(&name)) {
            return known.id;
        }
        let id = self.tast.add_label(Label { name, stmt: None });
        if let Some(state) = self.body.as_mut() {
            state.labels.insert(name, Labelled { id, defined: None, at: span });
        }
        id
    }

    /// The diagnostic for a label that something jumped to and nothing defined.
    ///
    /// gcc points at the function rather than at the jump, which is a choice about a message
    /// written at the end of a function and not about which one is the mistake. This points at
    /// the jump, since that is what has to be changed and since a `__label__` is reported at the
    /// end of a block that a function has no way to name.
    fn undefined_label(&mut self, label: Labelled) {
        let name = self.tast[label.id].name;
        let spelled = self.text(name).to_owned();
        self.report(
            Diagnostic::error(format!("label '{spelled}' used but not defined"), label.at)
                .with_code("E0629"),
        );
    }

    /// `goto *expr;`, GNU's computed goto.
    fn computed_goto(&mut self, target: ast::ExprId) -> Stmt {
        let at = self.ast.expr_span(target);
        let target = self.expr(target);
        let target = self.value(target);
        if self.is_poisoned(target) {
            return Stmt::Error;
        }
        let ty = self.tast[target].ty;
        // An integer is allowed through because a null pointer constant is one, and `goto *0;`
        // is what a macro expands to where the target is decided elsewhere.
        if !is_pointer(&self.types, ty) && !is_integer(&self.types, ty) {
            self.report(
                Diagnostic::error("computed goto must be pointer type", at).with_code("E0630"),
            );
            return Stmt::Error;
        }
        let void = self.types.pointer(self.types.void());
        let target = self.conv().to_type(target, void);
        Stmt::IndirectGoto(target)
    }

    /// `break;`, which needs a loop or a `switch` around it.
    fn break_stmt(&mut self, span: Span) -> Stmt {
        let inside =
            self.body.as_ref().is_some_and(|state| state.loops > 0 || !state.switches.is_empty());
        if inside {
            return Stmt::Break;
        }
        self.report(
            Diagnostic::error("break statement not within loop or switch", span).with_code("E0631"),
        );
        Stmt::Error
    }

    /// `continue;`, which needs a loop and is not satisfied by a `switch`.
    fn continue_stmt(&mut self, span: Span) -> Stmt {
        if self.body.as_ref().is_some_and(|state| state.loops > 0) {
            return Stmt::Continue;
        }
        self.report(
            Diagnostic::error("continue statement not within a loop", span).with_code("E0632"),
        );
        Stmt::Error
    }

    /// `return;` or `return expr;`, checked against the return type.
    ///
    /// Both mismatches are errors. They were warnings for as long as C has had prototypes, and
    /// gcc 14 turned them into errors along with the rest of `-Wreturn-mismatch`, because a
    /// function that returns nothing where a value was promised hands its caller whatever was in
    /// the return register.
    fn return_stmt(&mut self, value: Option<ast::ExprId>, span: Span) -> Stmt {
        let Some((ret, at)) = self.body.as_ref().map(|state| (state.ret, state.at)) else {
            return Stmt::Return(None);
        };
        let void = is_void(&self.types, ret);
        let Some(value) = value else {
            if !void {
                self.report(
                    Diagnostic::error(
                        "'return' with no value, in function returning non-void",
                        span,
                    )
                    .with_code("E0633")
                    .note("declared here".to_owned(), at),
                );
            }
            return Stmt::Return(None);
        };
        let where_from = self.ast.expr_span(value);
        let value = self.expr(value);
        let value = self.value(value);
        if !void {
            return Stmt::Return(Some(self.assign_to(ret, value, where_from, Target::Return)));
        }
        // C23 6.8.6.4 lets a function returning `void` say `return f();` where `f` returns
        // `void`, which is what a wrapper does and what gcc has always accepted.
        if !is_void(&self.types, self.tast[value].ty) && !self.is_poisoned(value) {
            self.report(
                Diagnostic::error("'return' with a value, in function returning void", where_from)
                    .with_code("E0634")
                    .note("declared here".to_owned(), at),
            );
        }
        let value = self.conv().to_void(value);
        Stmt::Return(Some(value))
    }

    /// The controlling expression of an `if`, a `while`, a `do` or a `for`.
    fn controlling(&mut self, cond: ast::ExprId) -> ExprId {
        let span = self.ast.expr_span(cond);
        let cond = self.expr(cond);
        self.condition(cond, span)
    }

    /// The innermost `switch` being checked.
    fn switches(&mut self) -> Option<&mut Switch> {
        self.body.as_mut()?.switches.last_mut()
    }

    /// A statement form that is recognised and not checked yet.
    fn statement_unsupported(&mut self, what: &str, span: Span) {
        self.report(
            Diagnostic::error(format!("{what} is not supported yet"), span).with_code("E0519"),
        );
    }
}

#[cfg(test)]
mod tests {
    use rucc_ast::{
        AttrList, Builtin, BuiltinSet, DeclSpecs, DeclSpecsId, Declarator, DeclaratorId, Derived,
        TypeSpec,
    };
    use rucc_base::Interner;
    use rucc_lex::{IntConstant, IntConstantType, Remarks};
    use rucc_session::Std;
    use rucc_target::{TargetInfo, Triple};
    use rucc_types::IntKind;

    use super::*;
    use crate::check::Context;
    use crate::print::Printer;

    /// The untyped tree a test checks, built by hand.
    ///
    /// The same shape as the fixtures next door and for the same reason: the checker borrows the
    /// interner for as long as it lives, so everything a test needs to name is named before the
    /// checker exists.
    struct Fixture {
        ast: rucc_ast::Ast,
        names: Interner,
        target: TargetInfo,
    }

    impl Fixture {
        fn new() -> Fixture {
            let target =
                TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().expect("a triple"));
            Fixture { ast: rucc_ast::Ast::new(), names: Interner::new(), target }
        }

        fn name(&mut self, text: &str) -> Symbol {
            self.names.intern(text)
        }

        fn int(&mut self, value: u128) -> ast::ExprId {
            let ty = IntConstantType::Standard(IntKind::Int);
            let id = self.ast.add_int(IntConstant { value, ty, remarks: Remarks::default() });
            self.ast.expr(ast::Expr::Int(id), Span::DUMMY)
        }

        fn use_name(&mut self, text: &str) -> ast::ExprId {
            let name = self.name(text);
            self.ast.expr(ast::Expr::Name(name), Span::DUMMY)
        }

        /// A specifier list naming a built-in type, as the keywords that were written.
        fn keywords(&mut self, written: &[BuiltinSet]) -> DeclSpecsId {
            let mut builtin = Builtin::NONE;
            for &keyword in written {
                builtin = builtin.add(keyword).expect("a keyword written once");
            }
            let mut specs = DeclSpecs::empty(Span::DUMMY);
            specs.ty = TypeSpec::Builtin(builtin);
            self.ast.add_specs(specs)
        }

        /// `int`, which is what most of these declarations are made of.
        fn int_specs(&mut self) -> DeclSpecsId {
            self.keywords(&[BuiltinSet::INT])
        }

        fn declarator(&mut self, name: Option<&str>, derived: &[Derived]) -> DeclaratorId {
            let name = name.map(|text| self.name(text));
            let derived = self.ast.add_derived_list(derived);
            self.ast.add_declarator(Declarator {
                name,
                name_span: Span::DUMMY,
                derived,
                span: Span::DUMMY,
            })
        }

        /// `int x;` and the like, as a statement.
        fn local(&mut self, specs: DeclSpecsId, name: &str) -> ast::DeclId {
            let declarator = self.declarator(Some(name), &[]);
            let item = ast::InitDeclarator {
                declarator,
                init: None,
                asm_label: None,
                attrs: AttrList::EMPTY,
                span: Span::DUMMY,
            };
            let declarators = self.ast.add_init_declarator_list(&[item]);
            self.ast.decl(ast::Decl::Var { specs, declarators }, Span::DUMMY)
        }

        /// `(ty)value`, which is how these tests write an expression of a type they choose.
        fn cast(&mut self, specs: DeclSpecsId, value: ast::ExprId) -> ast::ExprId {
            let declarator = self.declarator(None, &[]);
            let ty = self.ast.add_type_name(ast::TypeName { specs, declarator, span: Span::DUMMY });
            self.ast.expr(ast::Expr::Cast { ty, operand: value }, Span::DUMMY)
        }

        fn stmt(&mut self, stmt: ast::Stmt) -> ast::StmtId {
            self.ast.stmt(stmt, Span::DUMMY)
        }

        /// `{ ... }`, from the statements it holds.
        fn block(&mut self, body: &[ast::StmtId]) -> ast::StmtId {
            let body = self.ast.add_stmt_list(body);
            self.stmt(ast::Stmt::Compound(body))
        }

        /// `value;`.
        fn expr_stmt(&mut self, value: ast::ExprId) -> ast::StmtId {
            self.stmt(ast::Stmt::Expr(value))
        }

        /// `name: body`.
        fn labelled(&mut self, text: &str, body: Option<ast::StmtId>) -> ast::StmtId {
            let name = self.name(text);
            self.stmt(ast::Stmt::Label { name, body, attrs: AttrList::EMPTY })
        }

        /// `goto name;`.
        fn goto(&mut self, text: &str) -> ast::StmtId {
            let name = self.name(text);
            self.stmt(ast::Stmt::Goto(name))
        }

        /// `__label__ a, b;`.
        fn local_labels(&mut self, names: &[&str]) -> ast::StmtId {
            let names: Vec<Symbol> = names.iter().map(|text| self.name(text)).collect();
            let names = self.ast.add_symbol_list(&names);
            self.stmt(ast::Stmt::LocalLabels(names))
        }

        /// `case lo: body`, or GNU's `case lo ... hi: body`.
        fn case(&mut self, lo: u128, hi: Option<u128>, body: Option<ast::StmtId>) -> ast::StmtId {
            let lo = self.int(lo);
            let hi = hi.map(|hi| self.int(hi));
            self.stmt(ast::Stmt::Case { lo, hi, body })
        }

        /// `switch (scrutinee) { ... }`.
        fn switch(&mut self, scrutinee: ast::ExprId, body: &[ast::StmtId]) -> ast::StmtId {
            let body = self.block(body);
            self.stmt(ast::Stmt::Switch { scrutinee, body })
        }

        fn checker(&self) -> Checker<'_> {
            Checker::new(&self.ast, Context::new(&self.names, &self.target, Std::C23))
        }
    }

    /// The tree under one statement, which is what most assertions here are about.
    fn dump(checker: &Checker<'_>, id: StmtId) -> String {
        let mut printer = Printer::new(&checker.tast, &checker.types, checker.cx.names);
        printer.stmt(id);
        printer.finish()
    }

    /// What was reported, as the messages alone, notes included.
    fn messages(checker: &Checker<'_>) -> Vec<String> {
        checker
            .errors
            .diagnostics()
            .iter()
            .flat_map(|d| {
                std::iter::once(d.message.clone())
                    .chain(d.children.iter().map(|n| n.message.clone()))
            })
            .collect()
    }

    /// The one message that was reported, which is what most of these tests expect.
    fn message(checker: &Checker<'_>) -> String {
        let mut reported = messages(checker);
        assert_eq!(reported.len(), 1, "expected exactly one diagnostic, got {reported:?}");
        reported.pop().expect("one message")
    }

    /// What was reported, as the severity and the message of each, so that a test can say which
    /// of the two a diagnostic is. gcc 14 turned several of these from warnings into errors and
    /// the difference is the whole point of some of the tests below.
    fn reported(checker: &Checker<'_>) -> Vec<String> {
        checker
            .errors
            .diagnostics()
            .iter()
            .map(|d| format!("{}: {}", d.severity.as_str(), d.message))
            .collect()
    }

    #[test]
    fn a_block_is_a_scope_and_a_name_declared_in_one_is_gone_after_it() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let declared = f.local(specs, "x");
        let declared = f.stmt(ast::Stmt::Decl(declared));
        let inner = f.block(&[declared]);
        let use_x = f.use_name("x");
        let after = f.expr_stmt(use_x);
        let outer = f.block(&[inner, after]);

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, outer);

        assert_eq!(message(&c), "'x' undeclared (first use in this function)");
    }

    #[test]
    fn a_name_nobody_declared_is_reported_once_per_function_and_not_once_per_use() {
        // The wording promises it: `first use in this function` said three times is a sentence
        // arguing with itself. A misspelled name written in a loop body is one mistake, and one
        // message is what makes the next mistake in the file visible.
        let mut f = Fixture::new();
        let first = f.use_name("nope");
        let first = f.expr_stmt(first);
        let second = f.use_name("nope");
        let second = f.expr_stmt(second);
        let body = f.block(&[first, second]);

        let mut c = f.checker();
        let void = c.types.void();
        let previous = c.open_body(void, Span::DUMMY);
        c.check_stmt(void, body);
        c.close_body(previous);

        assert_eq!(message(&c), "'nope' undeclared (first use in this function)");
    }

    #[test]
    fn an_expression_statement_holds_the_value_and_not_a_conversion_of_it_to_void() {
        let mut f = Fixture::new();
        let one = f.int(1);
        let stmt = f.expr_stmt(one);

        let mut c = f.checker();
        let void = c.types.void();
        let id = c.check_stmt(void, stmt);

        assert_eq!(dump(&c, id), "expr\n  const 1 : int\n");
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_statement_expression_has_the_type_of_its_last_statement() {
        let mut f = Fixture::new();
        let one = f.int(1);
        let inner = f.expr_stmt(one);
        let body = f.block(&[inner]);
        let value = f.ast.expr(ast::Expr::StmtExpr(body), Span::DUMMY);
        let stmt = f.expr_stmt(value);

        let mut c = f.checker();
        let void = c.types.void();
        let id = c.check_stmt(void, stmt);

        assert_eq!(
            dump(&c, id),
            "expr\n  stmt-expr : int\n    block\n      expr\n        const 1 : int\n"
        );
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_statement_expression_that_ends_in_something_else_is_void() {
        let mut f = Fixture::new();
        let body = f.block(&[]);
        let value = f.ast.expr(ast::Expr::StmtExpr(body), Span::DUMMY);
        let stmt = f.expr_stmt(value);

        let mut c = f.checker();
        let void = c.types.void();
        let id = c.check_stmt(void, stmt);

        assert_eq!(dump(&c, id), "expr\n  stmt-expr : void\n    block\n");
        assert!(c.errors.is_empty());
    }

    #[test]
    fn the_declaration_in_a_for_clause_scopes_to_the_loop_and_not_to_what_follows() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let declared = f.local(specs, "i");
        let empty = f.stmt(ast::Stmt::Empty);
        let loop_stmt = f.stmt(ast::Stmt::For {
            init: ForInit::Decl(declared),
            cond: None,
            step: None,
            body: empty,
        });
        let use_i = f.use_name("i");
        let after = f.expr_stmt(use_i);
        let outer = f.block(&[loop_stmt, after]);

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, outer);

        assert_eq!(message(&c), "'i' undeclared (first use in this function)");
    }

    #[test]
    fn a_static_in_a_for_clause_is_accepted_and_only_pedantic_says_anything_about_it() {
        let mut f = Fixture::new();
        let mut specs = DeclSpecs::empty(Span::DUMMY);
        let builtin = Builtin::NONE.add(BuiltinSet::INT).expect("a keyword written once");
        specs.ty = TypeSpec::Builtin(builtin);
        specs.storage = Some(StorageClass::Static);
        let specs = f.ast.add_specs(specs);
        let declared = f.local(specs, "i");
        let empty = f.stmt(ast::Stmt::Empty);
        let loop_stmt = f.stmt(ast::Stmt::For {
            init: ForInit::Decl(declared),
            cond: None,
            step: None,
            body: empty,
        });

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, loop_stmt);
        assert!(c.errors.is_empty(), "got {:?}", messages(&c));

        let mut c = f.checker();
        c.cx.pedantic = true;
        let void = c.types.void();
        c.check_stmt(void, loop_stmt);
        assert_eq!(
            reported(&c),
            ["warning: declaration of static variable 'i' in 'for' loop initial declaration"]
        );
    }

    #[test]
    fn continue_needs_a_loop_and_is_not_satisfied_by_a_switch() {
        let mut f = Fixture::new();
        let one = f.int(1);
        let go_on = f.stmt(ast::Stmt::Continue);
        let case = f.stmt(ast::Stmt::Case { lo: one, hi: None, body: Some(go_on) });
        let scrutinee = f.int(0);
        let switch = f.switch(scrutinee, &[case]);

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, switch);

        assert_eq!(message(&c), "continue statement not within a loop");
    }

    #[test]
    fn break_is_satisfied_by_a_switch_and_reported_where_there_is_neither() {
        let mut f = Fixture::new();
        let stop = f.stmt(ast::Stmt::Break);
        let scrutinee = f.int(0);
        let switch = f.switch(scrutinee, &[stop]);
        let loose = f.stmt(ast::Stmt::Break);

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, switch);
        assert!(c.errors.is_empty(), "got {:?}", messages(&c));

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, loose);
        assert_eq!(message(&c), "break statement not within loop or switch");
    }

    #[test]
    fn a_goto_resolves_to_a_label_the_function_defines_further_down() {
        let mut f = Fixture::new();
        let jump = f.goto("done");
        let empty = f.stmt(ast::Stmt::Empty);
        let target = f.labelled("done", Some(empty));
        let body = f.block(&[jump, target]);

        let mut c = f.checker();
        let void = c.types.void();
        let id = c.check_stmt(void, body);

        assert_eq!(dump(&c, id), "block\n  goto #0 done\n  label #0 done\n    empty\n");
        assert!(c.errors.is_empty());
    }

    #[test]
    fn a_label_that_is_jumped_to_and_never_defined_is_reported_at_the_jump() {
        let mut f = Fixture::new();
        let jump = f.goto("away");
        let body = f.block(&[jump]);

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, body);

        assert_eq!(message(&c), "label 'away' used but not defined");
    }

    #[test]
    fn the_address_of_a_label_is_a_use_of_it_and_not_a_definition() {
        let mut f = Fixture::new();
        let away = f.name("away");
        let value = f.ast.expr(ast::Expr::LabelAddr(away), Span::DUMMY);
        let stmt = f.expr_stmt(value);

        let mut c = f.checker();
        let void = c.types.void();
        let id = c.check_stmt(void, stmt);

        assert_eq!(dump(&c, id), "expr\n  label-addr #0 away : void *\n");
        assert_eq!(message(&c), "label 'away' used but not defined");
    }

    #[test]
    fn one_label_defined_twice_is_an_error_that_points_at_the_first() {
        let mut f = Fixture::new();
        let first = f.labelled("here", None);
        let second = f.labelled("here", None);
        let body = f.block(&[first, second]);

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, body);

        assert_eq!(
            messages(&c),
            ["duplicate label 'here'", "previous definition of 'here' with type 'void'",]
        );
    }

    #[test]
    fn a_local_label_is_undone_when_its_block_ends_so_two_blocks_may_declare_one_name() {
        let mut f = Fixture::new();
        let sibling = |f: &mut Fixture| {
            let declared = f.local_labels(&["done"]);
            let jump = f.goto("done");
            let target = f.labelled("done", None);
            f.block(&[declared, jump, target])
        };
        let first = sibling(&mut f);
        let second = sibling(&mut f);
        let body = f.block(&[first, second]);

        let mut c = f.checker();
        let void = c.types.void();
        let id = c.check_stmt(void, body);

        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
        assert_eq!(
            dump(&c, id),
            "block\n  block\n    empty\n    goto #0 done\n    label #0 done\n      empty\n  \
             block\n    empty\n    goto #1 done\n    label #1 done\n      empty\n"
        );
    }

    #[test]
    fn a_local_label_that_nothing_defines_is_reported_when_its_block_ends() {
        let mut f = Fixture::new();
        let declared = f.local_labels(&["done"]);
        let jump = f.goto("done");
        let inner = f.block(&[declared, jump]);
        let target = f.labelled("done", None);
        let body = f.block(&[inner, target]);

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, body);

        assert_eq!(message(&c), "label 'done' used but not defined");
    }

    #[test]
    fn a_computed_goto_wants_something_that_could_be_an_address() {
        let mut f = Fixture::new();
        let specs = f.keywords(&[BuiltinSet::DOUBLE]);
        let zero = f.int(0);
        let target = f.cast(specs, zero);
        let stmt = f.stmt(ast::Stmt::GotoExpr(target));

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, stmt);

        assert_eq!(message(&c), "computed goto must be pointer type");
    }

    #[test]
    fn a_switch_on_something_that_is_not_an_integer_is_an_error() {
        let mut f = Fixture::new();
        let specs = f.keywords(&[BuiltinSet::DOUBLE]);
        let zero = f.int(0);
        let scrutinee = f.cast(specs, zero);
        let switch = f.switch(scrutinee, &[]);

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, switch);

        assert_eq!(message(&c), "switch quantity not an integer");
    }

    #[test]
    fn the_cases_of_a_switch_are_one_table_in_the_order_they_were_written() {
        let mut f = Fixture::new();
        let first = f.case(1, None, None);
        let second = f.case(4, Some(6), None);
        let default = f.stmt(ast::Stmt::Default { body: None });
        let scrutinee = f.int(0);
        let switch = f.switch(scrutinee, &[first, second, default]);

        let mut c = f.checker();
        let void = c.types.void();
        let id = c.check_stmt(void, switch);

        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
        assert_eq!(
            dump(&c, id),
            "switch\n  cond\n    const 0 : int\n  cases\n    case #0 1\n    case #1 4 ... 6\n    \
             default\n  body\n    block\n      case #0\n        empty\n      case #1\n        \
             empty\n      default\n        empty\n"
        );
    }

    #[test]
    fn a_case_that_covers_a_value_an_earlier_one_covers_is_a_duplicate() {
        let mut f = Fixture::new();
        let first = f.case(1, Some(3), None);
        let second = f.case(2, None, None);
        let scrutinee = f.int(0);
        let switch = f.switch(scrutinee, &[first, second]);

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, switch);

        assert_eq!(messages(&c), ["duplicate case value", "previously used here"]);
    }

    #[test]
    fn a_case_outside_a_switch_is_an_error_and_so_is_a_default() {
        let mut f = Fixture::new();
        let case = f.case(1, None, None);
        let default = f.stmt(ast::Stmt::Default { body: None });
        let body = f.block(&[case, default]);

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, body);

        assert_eq!(
            messages(&c),
            [
                "case label not within a switch statement",
                "'default' label not within a switch statement",
            ]
        );
    }

    #[test]
    fn a_case_label_that_is_not_a_constant_is_an_error() {
        let mut f = Fixture::new();
        let specs = f.int_specs();
        let declared = f.local(specs, "n");
        let declared = f.stmt(ast::Stmt::Decl(declared));
        let use_n = f.use_name("n");
        let case = f.stmt(ast::Stmt::Case { lo: use_n, hi: None, body: None });
        let scrutinee = f.int(0);
        let switch = f.switch(scrutinee, &[case]);
        let body = f.block(&[declared, switch]);

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, body);

        assert_eq!(message(&c), "case label does not reduce to an integer constant");
    }

    #[test]
    fn a_case_range_that_runs_backwards_is_empty() {
        let mut f = Fixture::new();
        let case = f.case(6, Some(4), None);
        let scrutinee = f.int(0);
        let switch = f.switch(scrutinee, &[case]);

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, switch);

        assert_eq!(reported(&c), ["warning: empty range specified"]);
    }

    #[test]
    fn a_case_is_measured_against_the_type_that_was_written_and_not_the_promoted_one() {
        let mut f = Fixture::new();
        let specs = f.keywords(&[BuiltinSet::CHAR]);
        let zero = f.int(0);
        let scrutinee = f.cast(specs, zero);
        let case = f.case(300, None, None);
        let switch = f.switch(scrutinee, &[case]);

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, switch);

        assert_eq!(reported(&c), ["warning: case label value exceeds maximum value for type"]);
    }

    #[test]
    fn two_defaults_in_one_switch_are_an_error_that_points_at_the_first() {
        let mut f = Fixture::new();
        let first = f.stmt(ast::Stmt::Default { body: None });
        let second = f.stmt(ast::Stmt::Default { body: None });
        let scrutinee = f.int(0);
        let switch = f.switch(scrutinee, &[first, second]);

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, switch);

        assert_eq!(
            messages(&c),
            ["multiple default labels in one switch", "this is the first default label"]
        );
    }

    #[test]
    fn a_nested_switch_keeps_its_cases_to_itself() {
        let mut f = Fixture::new();
        let inner_case = f.case(1, None, None);
        let inner_scrutinee = f.int(0);
        let inner = f.switch(inner_scrutinee, &[inner_case]);
        let outer_case = f.case(1, None, Some(inner));
        let outer_scrutinee = f.int(0);
        let outer = f.switch(outer_scrutinee, &[outer_case]);

        let mut c = f.checker();
        let void = c.types.void();
        let id = c.check_stmt(void, outer);

        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
        assert_eq!(
            dump(&c, id),
            "switch\n  cond\n    const 0 : int\n  cases\n    case #1 1\n  body\n    block\n      \
             case #1\n        switch\n          cond\n            const 0 : int\n          \
             cases\n            case #0 1\n          body\n            block\n              case \
             #0\n                empty\n"
        );
    }

    #[test]
    fn a_bare_return_from_a_function_that_promised_a_value_is_an_error() {
        let mut f = Fixture::new();
        let stmt = f.stmt(ast::Stmt::Return(None));

        let mut c = f.checker();
        let int = c.int();
        c.check_stmt(int, stmt);

        assert_eq!(reported(&c), ["error: 'return' with no value, in function returning non-void"]);
        assert_eq!(messages(&c).len(), 2, "the note is attached to it");
    }

    #[test]
    fn a_value_returned_from_a_function_returning_void_is_an_error() {
        let mut f = Fixture::new();
        let one = f.int(1);
        let stmt = f.stmt(ast::Stmt::Return(Some(one)));

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, stmt);

        assert_eq!(reported(&c), ["error: 'return' with a value, in function returning void"]);
    }

    #[test]
    fn a_void_value_returned_from_a_function_returning_void_is_what_a_wrapper_writes() {
        let mut f = Fixture::new();
        let specs = f.keywords(&[BuiltinSet::VOID]);
        let one = f.int(1);
        let value = f.cast(specs, one);
        let stmt = f.stmt(ast::Stmt::Return(Some(value)));

        let mut c = f.checker();
        let void = c.types.void();
        c.check_stmt(void, stmt);

        assert!(c.errors.is_empty(), "got {:?}", messages(&c));
    }

    #[test]
    fn a_returned_value_is_converted_to_the_return_type() {
        let mut f = Fixture::new();
        let one = f.int(1);
        let stmt = f.stmt(ast::Stmt::Return(Some(one)));

        let mut c = f.checker();
        let long = c.types.int(IntKind::Long);
        let id = c.check_stmt(long, stmt);

        assert_eq!(dump(&c, id), "return\n  convert arithmetic : long\n    const 1 : int\n");
        assert!(c.errors.is_empty());
    }
}
