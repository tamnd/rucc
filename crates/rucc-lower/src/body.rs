//! The per-function level of the walk: statements and expressions.
//!
//! Design: `spec/08-ir.md` section 8.9.
//!
//! # The cursor
//!
//! There is one place instructions are appended to, and it is [`Body::at`]. It is an option
//! because unreachable code exists: after a `return` there is no block to append to, and the
//! IR has no room for one, since the verifier rejects a block nothing branches to. So `at`
//! goes to [`None`] at a terminator and comes back when a construct starts a block that
//! something does branch to. A statement lowered while it is `None` is lowered to nothing.
//!
//! Every block a construct might need is created only when something is about to branch to it.
//! The join of an `if` whose arms both return is never created, and the block after a loop
//! nothing breaks out of and whose condition is `1` is never created either. That is not an
//! optimization, it is what keeps the CFG legal.
//!
//! # Where a variable lives
//!
//! A local is a value in [`Ssa`] unless something takes its address or it is not the kind of
//! thing a register holds, and then it is a stack slot. That decision is made once, before the
//! walk, by [`Scan`], because it has to be made for the whole function at once: the `alloca`
//! for a slot belongs in the entry block, and by the time the walk meets `&x` it is far too
//! late to put one there.

use std::collections::{HashMap, HashSet};

use rucc_ast::{BinaryOp, UnaryOp};
use rucc_base::float::Float as Real;
use rucc_diag::Span;
use rucc_ir::{
    Block, Builder, CallInfo, Extra, Flags, FloatPred, Func, InstData, IntPred, MemInfo, MemOrder,
    Opcode, Type, Value,
};
use rucc_sema::{Const, Conversion, DeclId, ExprId, ExprKind, Stmt, StmtId, StorageDuration, Tast};
use rucc_target::TargetInfo;
use rucc_types::{Qualifiers, TypeId, TypeKind, Types};

use crate::repr;
use crate::ssa::{Ssa, Var};
use crate::unit::Unit;

/// Builds the body of one function definition into `func`.
pub(crate) fn lower(unit: &mut Unit<'_>, decl: DeclId, func: &mut Func) {
    let tast = unit.tast;
    let Some(root) = tast[decl].body else { return };
    let params = tast[decl].params;
    let span = tast.decl_span(decl);
    if tast[params].len() != func.signature().params.len() {
        // A definition written without a prototype, `int f(a) int a; { }`, whose type says
        // nothing about what it takes. The entry block's parameters have to be the signature's
        // and here they are not, so the function is left as a declaration.
        unit.unsupported("a function definition without a prototype", span);
        return;
    }

    let entry = func.create_block();
    let address = repr::address_type(unit.target);
    let mut body = Body {
        unit,
        func,
        ssa: Ssa::new(address),
        at: Some(entry),
        vars: HashMap::new(),
        labels: HashMap::new(),
        loops: Vec::new(),
        next_var: 0,
        address,
    };
    body.ssa.seal(body.func, entry);

    // What the whole function needs decided before any of it is walked.
    let mut scan = Scan { tast, escaped: HashSet::new(), locals: Vec::new(), statics: Vec::new() };
    scan.stmt(root);
    let Scan { escaped, locals, statics, .. } = scan;
    for decl in statics {
        body.unit.local_static(decl);
    }

    // The slots first, so that every `alloca` is at the top of the entry block, and then the
    // parameters, whose stores have to come after the slots they store into.
    let params = tast[params].to_vec();
    for &param in &params {
        body.declare(param, escaped.contains(&param));
    }
    for &local in &locals {
        body.declare(local, escaped.contains(&local));
    }
    for (index, &param) in params.iter().enumerate() {
        let ty = func_param(body.func, index);
        let value = body.func.append_param(entry, ty);
        match body.vars.get(&param).copied() {
            Some(Local::Value(var)) => body.ssa.write(var, entry, value),
            Some(Local::Slot(slot)) => {
                let info = body.access(tast[param].ty);
                body.build(span).store(value, slot, info, Flags::NONE);
            }
            None => {}
        }
    }

    body.stmt(root);
    body.finish(decl, span);

    // A label is somewhere any `goto` in the function can branch to, so the block one starts
    // gets its last predecessor only when the last statement has been walked. A `case` was
    // sealed by its `switch`, which is why this asks rather than seals.
    let blocks: Vec<Block> = body.labels.values().copied().collect();
    for block in blocks {
        body.seal_once(block);
    }

    let Body { ssa, .. } = body;
    ssa.finish(func);
    prune(func);
}

/// Takes out the blocks nothing reaches, which is what a label in unreachable code can leave.
///
/// `int f(void) { return 1; spare: return 2; }` is a legal function with a block in it that
/// nothing branches to, and the verifier turns down a function with one of those in it. Which
/// labels turn out to be dead is not known until the whole body has been walked, since the
/// `goto` that reaches one is allowed to be the last statement in the function, so it is
/// answered here and not while the walk is going on.
fn prune(func: &mut Func) {
    let Some(entry) = func.entry() else { return };
    let mut reached = vec![false; func.counts().blocks];
    reached[entry.index()] = true;
    let mut stack = vec![entry];
    while let Some(block) = stack.pop() {
        let insts: Vec<rucc_ir::Inst> = func.insts(block).collect();
        for inst in insts {
            for call in func.target_list(inst).iter() {
                let to = func[call].block;
                if !reached[to.index()] {
                    reached[to.index()] = true;
                    stack.push(to);
                }
            }
        }
    }

    let blocks: Vec<Block> = func.blocks().collect();
    for block in blocks {
        if !reached[block.index()] {
            func.remove_block(block);
        }
    }
}

/// The type of one of a function's own parameters.
fn func_param(func: &Func, index: usize) -> Type {
    func.signature().params.get(index).copied().unwrap_or(Type::PTR)
}

/// Where a local variable lives.
#[derive(Debug, Clone, Copy)]
enum Local {
    /// In a register, as a value the SSA construction keeps track of.
    Value(Var),
    /// In a stack slot, whose address this is.
    Slot(Value),
}

/// An object the walk can read or write: either a variable or an address.
#[derive(Debug, Clone, Copy)]
struct Place {
    /// Where it is.
    at: Where,
    /// Its C type, which is what says how wide the access is and how aligned.
    ty: TypeId,
}

/// The two kinds of place there are.
#[derive(Debug, Clone, Copy)]
enum Where {
    /// A variable with no address, which a load and a store are a read and a write of.
    Var(Var),
    /// An address, which a load and a store are a load and a store of.
    Addr(Value),
}

/// One loop or `switch`, and where its `break` and its `continue` go.
#[derive(Debug, Clone, Copy)]
struct Frame {
    /// Which of the two it is, since a `continue` inside a `switch` belongs to the loop around
    /// it and a `break` there belongs to the `switch`.
    kind: FrameKind,
    /// Where `break` goes, created when the first one needs it.
    brk: Option<Block>,
    /// Where `continue` goes, created when the first one needs it.
    cont: Option<Block>,
}

/// What a frame was pushed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    /// A loop, which both statements leave.
    Loop,
    /// A `switch`, which only `break` leaves.
    Switch,
}

/// The walk over one function body.
struct Body<'a, 'u> {
    unit: &'a mut Unit<'u>,
    func: &'a mut Func,
    ssa: Ssa,
    /// The block instructions are appended to, absent in unreachable code.
    at: Option<Block>,
    vars: HashMap<DeclId, Local>,
    /// The block a labelled statement starts, for the labels met so far.
    labels: HashMap<StmtId, Block>,
    loops: Vec<Frame>,
    next_var: u32,
    /// The integer type an address is as wide as.
    address: Type,
}

impl std::fmt::Debug for Body<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Body").field("at", &self.at).field("vars", &self.vars.len()).finish()
    }
}

impl<'u> Body<'_, 'u> {
    // Building blocks.

    /// The typed tree.
    ///
    /// The reference is copied out of the unit rather than reborrowed from it, which is what
    /// makes reading a node and then building something not two borrows of the walk at once.
    fn tast(&self) -> &'u Tast {
        self.unit.tast
    }

    /// The type table, copied out for the same reason.
    fn types(&self) -> &'u Types {
        self.unit.types
    }

    /// What the target is, copied out for the same reason.
    fn target(&self) -> &'u TargetInfo {
        self.unit.target
    }

    /// The block being appended to.
    fn block(&self) -> Block {
        self.at.expect("nothing is built while the cursor is in unreachable code")
    }

    /// A builder on that block, with that span on everything it makes.
    fn build(&mut self, span: Span) -> Builder<'_> {
        let block = self.block();
        Builder::new(self.func, block).at(span)
    }

    /// A fresh block, which nothing branches to yet.
    fn new_block(&mut self) -> Block {
        self.func.create_block()
    }

    /// An unconditional branch to a block, which leaves the cursor in unreachable code.
    fn jump(&mut self, target: Block, span: Span) {
        let inst = self.build(span).jump(target, &[]);
        self.ssa.branch(self.func, inst);
        self.at = None;
    }

    /// A two-way branch, which leaves the cursor in unreachable code.
    fn br_if(&mut self, cond: Value, then: Block, otherwise: Block, span: Span) {
        let inst = self.build(span).br_if(cond, then, &[], otherwise, &[]);
        self.ssa.branch(self.func, inst);
        self.at = None;
    }

    /// A variable number nothing else uses, for a temporary the program did not declare.
    fn temp(&mut self) -> Var {
        let var = Var::new(self.next_var);
        self.next_var += 1;
        var
    }

    /// Decides where a local lives and makes its slot when it needs one.
    fn declare(&mut self, decl: DeclId, escaped: bool) {
        let tast = self.tast();
        let ty = tast[decl].ty;
        if tast[decl].duration != StorageDuration::Automatic {
            // A `static` in a function is a global, and a reference to it goes through its
            // name like any other. Nothing here holds it.
            return;
        }
        if repr::is_variable_length(self.types(), ty) {
            // The slot for one of these is not a fixed size `alloca`, it is a `stack_save`, a
            // multiplication and a `stack_restore` at the end of the block. The IR has the
            // instructions and the walk does not build them yet.
            let span = tast.decl_span(decl);
            self.unsupported("a variable length array", span);
            return;
        }
        let value = repr::value_type(self.types(), self.target(), ty);
        if !escaped && value.is_some() {
            let var = self.temp();
            self.vars.insert(decl, Local::Value(var));
            return;
        }
        let span = tast.decl_span(decl);
        let size = repr::size_of(self.types(), self.target(), ty);
        let align =
            tast[decl].alignment.unwrap_or_else(|| repr::align_of(self.types(), self.target(), ty));
        let slot = self.alloca(size, align, span);
        self.vars.insert(decl, Local::Slot(slot));
    }

    /// A stack slot of a fixed size, in the entry block where the verifier wants it.
    fn alloca(&mut self, size: u64, align: u32, span: Span) -> Value {
        let mut build = self.build(span);
        let info = MemInfo { size, align, order: MemOrder::NotAtomic, tbaa: None };
        let mem = build.func().add_mem(info);
        build.value(InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) }, Type::PTR)
    }

    /// How an object of that type is accessed: how wide and how aligned.
    fn access(&self, ty: TypeId) -> MemInfo {
        MemInfo {
            // Zero, because a load takes its width from the type it produces and a store from
            // the value it writes. The field is for the copies, which have no such type.
            size: 0,
            align: repr::align_of(self.types(), self.target(), ty),
            order: MemOrder::NotAtomic,
            tbaa: None,
        }
    }

    /// The flags an access to that type carries.
    fn flags(&self, ty: TypeId) -> Flags {
        if self.types().quals(ty).has(Qualifiers::VOLATILE) { Flags::VOLATILE } else { Flags::NONE }
    }

    /// The IR type of a C type, reporting once for one that has none.
    fn value_type(&mut self, ty: TypeId, span: Span) -> Type {
        match repr::value_type(self.types(), self.target(), ty) {
            Some(ty) => ty,
            None => {
                self.unit.unsupported("a value of this type", span);
                Type::PTR
            }
        }
    }

    /// A value to carry on with after something was reported.
    fn poison(&mut self, ty: Type, span: Span) -> Value {
        let address = self.address;
        if ty.is_ptr() {
            let zero = self.build(span).iconst(address, 0);
            return self.build(span).unary(Opcode::IntToPtr, zero, Type::PTR);
        }
        if ty.lane().is_float() {
            return self.build(span).fconst(ty, 0);
        }
        self.build(span).iconst(ty, 0)
    }

    // Statements.

    /// One statement.
    fn stmt(&mut self, id: StmtId) {
        if self.at.is_none() {
            self.unreachable_stmt(id);
            return;
        }
        let tast = self.tast();
        let span = tast.stmt_span(id);
        match tast[id] {
            Stmt::Error | Stmt::Empty => {}
            Stmt::Expr(expr) => {
                self.eval(expr);
            }
            Stmt::Block(list) => {
                for index in 0..tast[list].len() {
                    let stmt = tast[list][index];
                    self.stmt(stmt);
                }
            }
            Stmt::Decls(list) => {
                for index in 0..tast[list].len() {
                    let decl = tast[list][index];
                    self.init(decl);
                }
            }
            Stmt::If { cond, then, otherwise } => self.if_stmt(cond, then, otherwise, span),
            Stmt::While { cond, body } => self.while_stmt(cond, body, span),
            Stmt::DoWhile { body, cond } => self.do_while(body, cond, span),
            Stmt::For { init, cond, step, body } => self.for_stmt(init, cond, step, body, span),
            Stmt::Break => self.leave(true, span),
            Stmt::Continue => self.leave(false, span),
            Stmt::Return(value) => self.return_stmt(value, span),
            Stmt::Switch { cond, body, cases, default } => {
                self.switch_stmt(cond, body, cases, default, span);
            }
            Stmt::Case { body, .. } | Stmt::Default { body } | Stmt::Label { body, .. } => {
                self.labelled(body, span);
            }
            Stmt::Goto(label) => self.goto(label, span),
            Stmt::IndirectGoto(_) => self.unsupported("a computed goto", span),
        }
    }

    /// A statement in unreachable code, which is lowered to nothing unless there is a label in
    /// it.
    ///
    /// That label is the whole reason this exists. In `switch (x) { case 1: break; case 2: f(); }`
    /// there is no way to reach the second case except through the `switch`, and in
    /// `if (x) goto out; return 1; out: return 2;` there is no way to reach `out` except through
    /// the `goto`, so the walk arrives at both with no block to append to and has to start one
    /// rather than drop what follows. The statements a label can be reached through are walked,
    /// and the rest are dropped.
    ///
    /// A label somewhere the walk cannot start, which is inside a loop or an `if` that is itself
    /// unreachable, is reported. Control there jumps into the middle of a construct the walk only
    /// knows how to build from the top, and lowering it to the construct without the jump would
    /// be a miscompile. Duff's device is not this: there the `do` is what the first `case`
    /// labels, so it is reached from the top and the labels inside it are ordinary edges.
    fn unreachable_stmt(&mut self, id: StmtId) {
        let tast = self.tast();
        match tast[id] {
            Stmt::Block(list) => {
                for index in 0..tast[list].len() {
                    let stmt = tast[list][index];
                    self.stmt(stmt);
                }
            }
            Stmt::Case { body, .. } | Stmt::Default { body } | Stmt::Label { body, .. } => {
                let span = tast.stmt_span(id);
                self.labelled(body, span);
            }
            _ => {
                if holds_a_label(tast, id, true) {
                    let span = tast.stmt_span(id);
                    self.unsupported("a label control cannot fall into", span);
                }
            }
        }
    }

    /// `name:`, `case value:` or `default:`, which is a block whatever reaches the label
    /// branches to.
    ///
    /// It is a block even when control also falls into it from the statement before, because
    /// something branches to it and a block is what a branch needs. The key is the statement the
    /// label labels, which is what the case table and the label table both hold, so the block a
    /// `switch` or a `goto` was built with and the block the walk arrives at are the same one.
    fn labelled(&mut self, body: StmtId, span: Span) {
        let block = self.label_block(body);
        if self.at.is_some() {
            self.jump(block, span);
        }
        self.at = Some(block);
        self.stmt(body);
    }

    /// `goto name;`, which is a jump to the block the label starts.
    ///
    /// The block is made here when the `goto` comes first, which is the common direction, and
    /// found when the label does. Either way it is one entry in the same table, so a label with
    /// twenty `goto`s to it is one block with twenty edges into it.
    fn goto(&mut self, label: rucc_sema::LabelId, span: Span) {
        let Some(body) = self.tast()[label].stmt else {
            // A label used and never defined, which the checking reported. There is nowhere to
            // jump to, and what follows is as unreachable as it would have been.
            self.at = None;
            return;
        };
        let block = self.label_block(body);
        self.jump(block, span);
        self.at = None;
    }

    /// The block a labelled statement starts, made the first time the `switch`, the `goto` or
    /// the walk asks for it.
    fn label_block(&mut self, body: StmtId) -> Block {
        match self.labels.get(&body) {
            Some(&block) => block,
            None => {
                let block = self.new_block();
                self.labels.insert(body, block);
                block
            }
        }
    }

    /// `switch (cond) body`.
    ///
    /// The cases are in a table on the statement rather than in the body, so the targets are
    /// known before the body is walked and the branch can be emitted first. The body is then
    /// walked with the cursor in unreachable code, which is what it is: the statements between
    /// the `switch` and its first label are reached by nothing, and every label starts a block
    /// the branch above already points at.
    fn switch_stmt(
        &mut self,
        cond: ExprId,
        body: StmtId,
        cases: rucc_sema::CaseList,
        default: Option<StmtId>,
        span: Span,
    ) {
        let value = self.value(cond);
        let ty = self.func[value].ty;
        let tast = self.tast();
        let table = tast[cases].to_vec();
        let mut blocks = Vec::with_capacity(table.len() + 1);

        // A `switch` with no label in it at all is the controlling expression and nothing else.
        // Control cannot get into the body, so it is walked as the unreachable code it is, and
        // the block after the `switch` is the block the `switch` was reached in rather than a
        // new one nothing would ever branch to twice.
        if table.is_empty() && default.is_none() {
            let resume = self.at;
            self.at = None;
            self.loops.push(Frame { kind: FrameKind::Switch, brk: None, cont: None });
            self.stmt(body);
            self.loops.pop();
            self.at = resume;
            return;
        }

        // Where a value that matches nothing goes, and where a `break` goes. They are the same
        // block when there is no `default:`, and the one after the `switch` is then reached by
        // the branch itself rather than only by whatever breaks out.
        let (default_block, mut after) = match default {
            Some(stmt) => (self.label_block(stmt), None),
            None => {
                let block = self.new_block();
                (block, Some(block))
            }
        };
        if default.is_some() {
            blocks.push(default_block);
        }

        // GNU's `case 1 ... 9` is a range, and a range is not something a jump table holds: the
        // values in it can be more numerous than the instructions in the function. Each one is
        // tested for before the branch instead, as one subtraction and one unsigned comparison,
        // which is the test for `low <= value && value <= high` in two instructions rather than
        // four. The rest go in the table.
        let mut singles = Vec::with_capacity(table.len());
        for case in &table {
            let block = self.label_block(case.body);
            blocks.push(block);
            if case.low == case.high {
                singles.push((case.low, block));
                continue;
            }
            let next = self.new_block();
            let low = self.build(span).iconst(ty, case.low);
            let base = self.build(span).binary(Opcode::Sub, value, low, Flags::NONE);
            let width = self.build(span).iconst(ty, case.high.wrapping_sub(case.low));
            let inside = self.build(span).icmp(IntPred::Ule, base, width);
            self.br_if(inside, block, next, span);
            self.ssa.seal(self.func, next);
            self.at = Some(next);
        }

        // With nothing left for the table, which is a `switch` whose cases are all ranges or
        // one with no cases at all, what is left is where everything else goes.
        if singles.is_empty() {
            self.jump(default_block, span);
        } else {
            let inst = self.build(span).switch(value, default_block, &singles);
            self.ssa.branch(self.func, inst);
            self.at = None;
        }

        self.loops.push(Frame { kind: FrameKind::Switch, brk: after, cont: None });
        self.stmt(body);
        let frame = self.loops.pop().expect("the frame that was just pushed");
        after = frame.brk;

        // Falling off the end of the body leaves the `switch` the same way `break` does.
        if self.at.is_some() {
            let block = match after {
                Some(block) => block,
                None => {
                    let block = self.new_block();
                    after = Some(block);
                    block
                }
            };
            self.jump(block, span);
        }

        // Every edge into a case has been made now: the branch above made one and falling out
        // of the case before it made the other, which is why none of these could be sealed any
        // earlier and why a variable a case assigns is read correctly in the case after it.
        for &block in &blocks {
            self.seal_once(block);
        }
        if let Some(block) = after {
            self.seal_once(block);
        }
        self.at = after;
    }

    /// Says a block has all the predecessors it is going to have, unless that has been said.
    ///
    /// Sealing is once per block and the case table is not something this file builds, so the
    /// question is asked rather than assumed. Two labels on one statement would otherwise be a
    /// panic in the compiler over a program that is perfectly legal.
    fn seal_once(&mut self, block: Block) {
        if !self.ssa.is_sealed(block) {
            self.ssa.seal(self.func, block);
        }
    }

    /// `if (cond) then else otherwise`.
    fn if_stmt(&mut self, cond: ExprId, then: StmtId, otherwise: Option<StmtId>, span: Span) {
        let cond = self.condition(cond);
        let then_block = self.new_block();
        let else_block = self.new_block();
        self.br_if(cond, then_block, else_block, span);
        self.ssa.seal(self.func, then_block);
        self.ssa.seal(self.func, else_block);

        let mut join = None;
        self.at = Some(then_block);
        self.stmt(then);
        self.leave_arm(&mut join, span);

        self.at = Some(else_block);
        if let Some(otherwise) = otherwise {
            self.stmt(otherwise);
        }
        self.leave_arm(&mut join, span);

        self.at = join;
        if let Some(join) = join {
            self.ssa.seal(self.func, join);
        }
    }

    /// The end of one arm of an `if`, which branches to the join and makes it if it has to.
    fn leave_arm(&mut self, join: &mut Option<Block>, span: Span) {
        if self.at.is_none() {
            return;
        }
        let target = match *join {
            Some(block) => block,
            None => {
                let block = self.new_block();
                *join = Some(block);
                block
            }
        };
        self.jump(target, span);
    }

    /// `while (cond) body`.
    fn while_stmt(&mut self, cond: ExprId, body: StmtId, span: Span) {
        let header = self.new_block();
        self.jump(header, span);
        self.at = Some(header);

        let value = self.condition(cond);
        let inside = self.new_block();
        let after = self.new_block();
        self.br_if(value, inside, after, span);
        self.ssa.seal(self.func, inside);

        self.at = Some(inside);
        self.loops.push(Frame { kind: FrameKind::Loop, brk: Some(after), cont: Some(header) });
        self.stmt(body);
        self.loops.pop();
        if self.at.is_some() {
            self.jump(header, span);
        }

        // Every edge into the header has been made now, which is the whole reason the header
        // was left unsealed: the back edge is the one a variable the loop changes arrives on.
        self.ssa.seal(self.func, header);
        self.ssa.seal(self.func, after);
        self.at = Some(after);
    }

    /// `do body while (cond);`.
    fn do_while(&mut self, body: StmtId, cond: ExprId, span: Span) {
        let inside = self.new_block();
        self.jump(inside, span);
        self.at = Some(inside);

        self.loops.push(Frame { kind: FrameKind::Loop, brk: None, cont: None });
        self.stmt(body);
        let frame = self.loops.pop().expect("the frame that was just pushed");

        // The test is the continue target, and it exists only if something reaches it: a body
        // that ends in `return` and has no `continue` never tests the condition again.
        let test = match (frame.cont, self.at.is_some()) {
            (Some(block), _) => Some(block),
            (None, true) => Some(self.new_block()),
            (None, false) => None,
        };
        if let Some(test) = test {
            if self.at.is_some() {
                self.jump(test, span);
            }
            self.ssa.seal(self.func, test);
            self.at = Some(test);
            let value = self.condition(cond);
            let after = match frame.brk {
                Some(block) => block,
                None => self.new_block(),
            };
            self.br_if(value, inside, after, span);
            self.ssa.seal(self.func, inside);
            self.ssa.seal(self.func, after);
            self.at = Some(after);
            return;
        }
        self.ssa.seal(self.func, inside);
        self.at = frame.brk;
        if let Some(after) = self.at {
            self.ssa.seal(self.func, after);
        }
    }

    /// `for (init; cond; step) body`.
    fn for_stmt(
        &mut self,
        init: Option<StmtId>,
        cond: Option<ExprId>,
        step: Option<ExprId>,
        body: StmtId,
        span: Span,
    ) {
        if let Some(init) = init {
            self.stmt(init);
        }
        if self.at.is_none() {
            return;
        }
        let header = self.new_block();
        self.jump(header, span);
        self.at = Some(header);

        // With no condition the header is the top of the body, and `for (;;)` leaves through a
        // `break` or not at all.
        let mut after = None;
        if let Some(cond) = cond {
            let value = self.condition(cond);
            let inside = self.new_block();
            let exit = self.new_block();
            self.br_if(value, inside, exit, span);
            self.ssa.seal(self.func, inside);
            self.at = Some(inside);
            after = Some(exit);
        }

        // The step is the continue target when there is one, and the header is when there is
        // not, since a `continue` in that case has nothing to run before the next test.
        let cont = if step.is_some() { None } else { Some(header) };
        self.loops.push(Frame { kind: FrameKind::Loop, brk: after, cont });
        self.stmt(body);
        let frame = self.loops.pop().expect("the frame that was just pushed");

        if let Some(step) = step {
            let block = match (frame.cont, self.at.is_some()) {
                (Some(block), _) => Some(block),
                (None, true) => Some(self.new_block()),
                (None, false) => None,
            };
            if let Some(block) = block {
                if self.at.is_some() {
                    self.jump(block, span);
                }
                self.ssa.seal(self.func, block);
                self.at = Some(block);
                self.eval(step);
                self.jump(header, span);
            }
        } else if self.at.is_some() {
            self.jump(header, span);
        }

        self.ssa.seal(self.func, header);
        self.at = frame.brk.or(after);
        if let Some(after) = self.at {
            self.ssa.seal(self.func, after);
        }
    }

    /// `break;` or `continue;`.
    ///
    /// A `break` leaves the innermost frame whatever it is, and a `continue` leaves the
    /// innermost loop, which is not the same thing inside a `switch` inside a loop.
    fn leave(&mut self, breaking: bool, span: Span) {
        let found = if breaking {
            self.loops.len().checked_sub(1)
        } else {
            self.loops.iter().rposition(|frame| frame.kind == FrameKind::Loop)
        };
        let Some(frame) = found else {
            // A `break` outside a loop is a diagnostic the checking already made.
            self.at = None;
            return;
        };
        let existing = if breaking { self.loops[frame].brk } else { self.loops[frame].cont };
        let target = match existing {
            Some(block) => block,
            None => {
                let block = self.new_block();
                if breaking {
                    self.loops[frame].brk = Some(block);
                } else {
                    self.loops[frame].cont = Some(block);
                }
                block
            }
        };
        self.jump(target, span);
    }

    /// `return;` or `return expr;`.
    fn return_stmt(&mut self, value: Option<ExprId>, span: Span) {
        let values = match value {
            Some(expr) => match self.eval(expr) {
                Some(value) => vec![value],
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        self.build(span).ret(&values);
        self.at = None;
    }

    /// The end of the body, where falling off the end has to become a terminator.
    fn finish(&mut self, decl: DeclId, span: Span) {
        if self.at.is_none() {
            return;
        }
        if self.func.signature().returns.is_empty() {
            self.build(span).ret(&[]);
            self.at = None;
            return;
        }
        let name = self.tast()[decl].name;
        let main = name.is_some_and(|name| self.unit.names.resolve(name) == "main");
        if main {
            // 5.1.2.2.3: reaching the closing brace of `main` returns zero.
            let ty = self.func.signature().returns[0];
            let zero = self.build(span).iconst(ty, 0);
            self.build(span).ret(&[zero]);
            self.at = None;
            return;
        }
        // Falling off the end of a function that returns something and then using the value is
        // undefined, so there is nothing to return and nothing to invent.
        self.build(span).unreachable();
        self.at = None;
    }

    /// The initializer of one declaration in a declaration statement.
    fn init(&mut self, decl: DeclId) {
        let tast = self.tast();
        let ty = tast[decl].ty;
        let Some(init) = tast[decl].init else { return };
        if tast[decl].duration != StorageDuration::Automatic {
            // The image of a `static` was built when the global was, at translation time.
            return;
        }
        let span = tast.decl_span(decl);
        let entries = tast[init].to_vec();
        let place = match self.vars.get(&decl).copied() {
            Some(Local::Value(var)) => Place { at: Where::Var(var), ty },
            Some(Local::Slot(slot)) => Place { at: Where::Addr(slot), ty },
            None => return,
        };

        if let Where::Var(_) = place.at {
            // A scalar in a register, which one initializer entry fills exactly.
            if let Some(entry) = entries.first() {
                if let Some(value) = self.eval(entry.value) {
                    self.write(place, value, span);
                }
            }
            return;
        }

        let size = repr::size_of(self.types(), self.target(), ty);
        let mut covered = 0;
        for entry in &entries {
            covered += self.stored_size(entry.value);
        }
        if covered < size {
            // What the initializer does not name is zero, and the padding between members is
            // zero as well, which is what makes a partly initialized structure comparable byte
            // for byte with another one.
            let slot = self.address_of(place, span);
            let zero = self.build(span).iconst(Type::int(8), 0);
            let align = repr::align_of(self.types(), self.target(), ty);
            let info = MemInfo { size, align, order: MemOrder::NotAtomic, tbaa: None };
            let mut build = self.build(span);
            let mem = build.func().add_mem(info);
            let args = build.func().push_values(&[slot, zero]);
            build.inst(
                InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Memset) },
                &[],
            );
        }
        for entry in entries {
            if entry.bit_width != 0 {
                self.unsupported("a bit-field", span);
                continue;
            }
            self.store_entry(place, entry.offset, entry.value, span);
        }
    }

    /// How many bytes one initializer entry writes.
    fn stored_size(&mut self, value: ExprId) -> u64 {
        let tast = self.tast();
        let ty = tast[value].ty;
        let size = repr::size_of(self.types(), self.target(), ty);
        match tast[value].kind {
            // A string literal shorter than the array it initializes writes what it has, and
            // the rest of the array is zero.
            ExprKind::Str(id) => size.min(tast[id].bytes(self.unit.target).len() as u64),
            _ => size,
        }
    }

    /// One entry of an initializer, at its offset into the object.
    fn store_entry(&mut self, place: Place, offset: u64, value: ExprId, span: Span) {
        let tast = self.tast();
        let ty = tast[value].ty;
        let base = self.address_of(place, span);
        let addr = self.offset(base, offset, span);
        if let ExprKind::Str(id) = tast[value].kind {
            let bytes = tast[id].bytes(self.unit.target).len() as u64;
            let size = bytes.min(repr::size_of(self.types(), self.target(), ty));
            let symbol = self.unit.string(id);
            let source = self.global_addr(symbol, span);
            self.memcpy(addr, source, size, 1, span);
            return;
        }
        if repr::value_type(self.types(), self.target(), ty).is_none() {
            // An aggregate initializing part of an aggregate, which is `struct p = q;` and
            // `struct p = (struct point){ 1, 2 };`. It is a copy rather than a store, because
            // an aggregate is not a value the IR can hold.
            let source = self.place(value);
            let source = self.address_of(source, span);
            let size = repr::size_of(self.types(), self.target(), ty);
            let align = repr::align_of(self.types(), self.target(), ty);
            self.memcpy(addr, source, size, align, span);
            return;
        }
        let Some(value) = self.eval(value) else { return };
        let info = self.access(ty);
        let flags = self.flags(ty);
        self.build(span).store(value, addr, info, flags);
    }

    /// A copy of a fixed number of bytes from one address to another.
    fn memcpy(&mut self, to: Value, from: Value, size: u64, align: u32, span: Span) {
        if size == 0 {
            return;
        }
        let info = MemInfo { size, align, order: MemOrder::NotAtomic, tbaa: None };
        let mut build = self.build(span);
        let mem = build.func().add_mem(info);
        let args = build.func().push_values(&[to, from]);
        build.inst(InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Memcpy) }, &[]);
    }

    // Places.

    /// Where an lvalue is.
    fn place(&mut self, expr: ExprId) -> Place {
        let tast = self.tast();
        let span = tast.expr_span(expr);
        let ty = tast[expr].ty;
        match tast[expr].kind {
            ExprKind::Decl(decl) => match self.vars.get(&decl).copied() {
                Some(Local::Value(var)) => Place { at: Where::Var(var), ty },
                Some(Local::Slot(slot)) => Place { at: Where::Addr(slot), ty },
                None => {
                    // Not a local, so it is an object with a name the linker knows: a global,
                    // a `static` in some function, or a function.
                    let symbol = self.unit.symbol_of(decl);
                    let addr = self.global_addr(symbol, span);
                    Place { at: Where::Addr(addr), ty }
                }
            },
            ExprKind::Str(id) => {
                let symbol = self.unit.string(id);
                let addr = self.global_addr(symbol, span);
                Place { at: Where::Addr(addr), ty }
            }
            ExprKind::Unary { op: UnaryOp::Deref, operand } => {
                let addr = self.value(operand);
                Place { at: Where::Addr(addr), ty }
            }
            ExprKind::Member { base, field } => self.member(base, field, ty, span),
            ExprKind::Subscript { base, index } => {
                let addr = self.element(base, index, ty, span);
                Place { at: Where::Addr(addr), ty }
            }
            // An aggregate is read by address rather than by value, so the conversion that
            // reads one is the identity and the place under it is the answer.
            ExprKind::Convert { kind: Conversion::Lvalue, operand } => self.place(operand),
            ExprKind::CompoundLiteral(decl) => self.literal(decl, ty),
            _ => {
                self.unsupported("this as the target of an assignment", span);
                let addr = self.poison(Type::PTR, span);
                Place { at: Where::Addr(addr), ty }
            }
        }
    }

    /// `(T){ ... }`, which is an object like any other and is initialized where it is written.
    ///
    /// Written where it is evaluated rather than once at the top of the function, because an
    /// evaluation of one of these is what initializes it: the same literal in a loop is one
    /// object that starts again each time round, which is what its initializer says.
    fn literal(&mut self, decl: DeclId, ty: TypeId) -> Place {
        match self.vars.get(&decl).copied() {
            Some(Local::Value(var)) => {
                self.init(decl);
                Place { at: Where::Var(var), ty }
            }
            Some(Local::Slot(slot)) => {
                self.init(decl);
                Place { at: Where::Addr(slot), ty }
            }
            None => {
                // One with static storage, which is a global and was written at the module
                // level with its image already in it.
                let span = self.tast().decl_span(decl);
                let symbol = self.unit.symbol_of(decl);
                let addr = self.global_addr(symbol, span);
                Place { at: Where::Addr(addr), ty }
            }
        }
    }

    /// `base.field`, which is the base's address plus the member's offset.
    fn member(&mut self, base: ExprId, field: u32, ty: TypeId, span: Span) -> Place {
        let place = self.place(base);
        let addr = self.address_of(place, span);
        let record = self.types().canonical(self.tast()[base].ty);
        let TypeKind::Record(id) = self.types().kind(record) else {
            return Place { at: Where::Addr(addr), ty };
        };
        let Some(member) = self.types().record_info(id).fields.get(field as usize).copied() else {
            return Place { at: Where::Addr(addr), ty };
        };
        if member.is_bit_field() {
            self.unsupported("a bit-field", span);
            return Place { at: Where::Addr(addr), ty };
        }
        let addr = self.offset(addr, member.byte_offset(), span);
        Place { at: Where::Addr(addr), ty }
    }

    /// `base[index]`, where the base is already a pointer to the element type.
    fn element(&mut self, base: ExprId, index: ExprId, ty: TypeId, span: Span) -> Value {
        let pointer = self.value(base);
        let steps = self.value(index);
        let size = repr::size_of(self.types(), self.target(), ty);
        let signed = repr::is_signed(self.types(), self.target(), self.tast()[index].ty);
        self.step(pointer, steps, signed, size, false, span)
    }

    /// The address of a place, which every object except a variable in a register has.
    fn address_of(&mut self, place: Place, span: Span) -> Value {
        match place.at {
            Where::Addr(addr) => addr,
            Where::Var(_) => {
                // Nothing should ask: a variable whose address is taken was put in a slot
                // before the walk started.
                self.unsupported("the address of this object", span);
                self.poison(Type::PTR, span)
            }
        }
    }

    /// The address of a global.
    fn global_addr(&mut self, symbol: rucc_base::Symbol, span: Span) -> Value {
        self.build(span).value(
            InstData { extra: Extra::Symbol(symbol), ..InstData::new(Opcode::GlobalAddr) },
            Type::PTR,
        )
    }

    /// An address a constant number of bytes further on.
    fn offset(&mut self, addr: Value, bytes: u64, span: Span) -> Value {
        if bytes == 0 {
            return addr;
        }
        let address = self.address;
        let mut build = self.build(span);
        let amount = build.iconst(address, bytes as i128);
        let args = build.func().push_values(&[addr, amount]);
        build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
    }

    /// An address a number of elements further on, or back when `back` is set.
    fn step(
        &mut self,
        addr: Value,
        steps: Value,
        signed: bool,
        size: u64,
        back: bool,
        span: Span,
    ) -> Value {
        let address = self.address;
        let mut amount = self.widen(steps, signed, address, span);
        if size != 1 {
            let mut build = self.build(span);
            let scale = build.iconst(address, size as i128);
            amount = build.binary(Opcode::Mul, amount, scale, Flags::NSW);
        }
        if back {
            let mut build = self.build(span);
            let zero = build.iconst(address, 0);
            amount = build.binary(Opcode::Sub, zero, amount, Flags::NONE);
        }
        let mut build = self.build(span);
        let args = build.func().push_values(&[addr, amount]);
        build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
    }

    /// An integer in another integer's width, which is the only conversion an index needs.
    fn widen(&mut self, value: Value, signed: bool, to: Type, span: Span) -> Value {
        let from = self.func[value].ty;
        match from.bits().cmp(&to.bits()) {
            std::cmp::Ordering::Equal => value,
            std::cmp::Ordering::Greater => self.build(span).unary(Opcode::Trunc, value, to),
            std::cmp::Ordering::Less => {
                let opcode = if signed { Opcode::SExt } else { Opcode::ZExt };
                self.build(span).unary(opcode, value, to)
            }
        }
    }

    /// Reads a place.
    fn read(&mut self, place: Place, span: Span) -> Option<Value> {
        let ty = repr::value_type(self.types(), self.target(), place.ty)?;
        if let TypeKind::Atomic(_) = self.types().kind(self.types().canonical(place.ty)) {
            self.unsupported("an access to an atomic object", span);
        }
        match place.at {
            Where::Var(var) => {
                let block = self.block();
                Some(self.ssa.read(self.func, var, block, ty))
            }
            Where::Addr(addr) => {
                let info = self.access(place.ty);
                let flags = self.flags(place.ty);
                Some(self.build(span).load(ty, addr, info, flags))
            }
        }
    }

    /// Writes a place.
    fn write(&mut self, place: Place, value: Value, span: Span) {
        if let TypeKind::Atomic(_) = self.types().kind(self.types().canonical(place.ty)) {
            self.unsupported("an access to an atomic object", span);
        }
        match place.at {
            Where::Var(var) => {
                let block = self.block();
                self.ssa.write(var, block, value);
            }
            Where::Addr(addr) => {
                let info = self.access(place.ty);
                let flags = self.flags(place.ty);
                self.build(span).store(value, addr, info, flags);
            }
        }
    }

    // Expressions.

    /// The value of an expression, which is [`None`] only when it has none.
    fn eval(&mut self, expr: ExprId) -> Option<Value> {
        let tast = self.tast();
        let span = tast.expr_span(expr);
        let ty = tast[expr].ty;
        match tast[expr].kind {
            ExprKind::Error => {
                let ty = repr::value_type(self.types(), self.target(), ty)?;
                Some(self.poison(ty, span))
            }
            ExprKind::Const(id) => self.constant(tast[id], ty, span),
            ExprKind::Str(_)
            | ExprKind::Decl(_)
            | ExprKind::Member { .. }
            | ExprKind::Subscript { .. }
            | ExprKind::CompoundLiteral(_) => {
                let place = self.place(expr);
                self.read(place, span)
            }
            ExprKind::Call { callee, args } => self.call(callee, args, span),
            ExprKind::Unary { op, operand } => self.unary(op, operand, ty, span),
            ExprKind::Binary { op, lhs, rhs } => self.binary(op, lhs, rhs, ty, span),
            ExprKind::Assign { op, computation, lhs, rhs } => {
                self.assign(op, computation, lhs, rhs, span)
            }
            ExprKind::Cond { cond, then, otherwise } => {
                self.conditional(cond, then, otherwise, ty, span)
            }
            ExprKind::Comma { lhs, rhs } => {
                self.eval(lhs);
                self.eval(rhs)
            }
            ExprKind::Cast(operand) => {
                let from = tast[operand].ty;
                if matches!(self.types().kind(self.types().canonical(ty)), TypeKind::Void) {
                    self.eval(operand);
                    return None;
                }
                let value = self.eval(operand)?;
                Some(self.coerce(value, from, ty, span))
            }
            ExprKind::Convert { kind, operand } => self.convert(kind, operand, ty, span),
            ExprKind::StmtExpr(_) => {
                self.unsupported("a statement expression", span);
                let ty = repr::value_type(self.types(), self.target(), ty)?;
                Some(self.poison(ty, span))
            }
            ExprKind::LabelAddr(_) => {
                self.unsupported("the address of a label", span);
                Some(self.poison(Type::PTR, span))
            }
            ExprKind::VaArg { .. } => {
                self.unsupported("va_arg", span);
                let ty = repr::value_type(self.types(), self.target(), ty)?;
                Some(self.poison(ty, span))
            }
        }
    }

    /// The value of an expression that has to have one.
    fn value(&mut self, expr: ExprId) -> Value {
        let span = self.tast().expr_span(expr);
        let ty = self.tast()[expr].ty;
        match self.eval(expr) {
            Some(value) => value,
            None => {
                let ty = self.value_type(ty, span);
                self.poison(ty, span)
            }
        }
    }

    /// A condition, which is one bit however it was written.
    fn condition(&mut self, expr: ExprId) -> Value {
        self.bit(expr)
    }

    /// An expression as the one bit that says whether it is true.
    ///
    /// The point of going through here rather than through [`Body::eval`] is what C says the
    /// type of a comparison is, which is `int` and not `bool`. Lowering `a < b` on its own
    /// widens the bit the comparison produced, and lowering `if (a < b)` would then narrow it
    /// straight back by comparing it against zero. Asking for the bit is what skips both.
    fn bit(&mut self, expr: ExprId) -> Value {
        let tast = self.tast();
        let span = tast.expr_span(expr);
        match tast[expr].kind {
            ExprKind::Binary { op, lhs, rhs } if op.is_comparison() => {
                let operand = tast[lhs].ty;
                let left = self.value(lhs);
                let right = self.value(rhs);
                self.compare(op, left, right, operand, span)
            }
            ExprKind::Binary { op: op @ (BinaryOp::LogAnd | BinaryOp::LogOr), lhs, rhs } => {
                self.short_circuit(op, lhs, rhs, span)
            }
            ExprKind::Unary { op: UnaryOp::Not, operand } => {
                let bit = self.bit(operand);
                let mut build = self.build(span);
                let one = build.iconst(Type::I1, 1);
                build.binary(Opcode::Xor, bit, one, Flags::NONE)
            }
            // The conversion the checking wrote on a condition, which is this question asked
            // one node further down.
            ExprKind::Convert { kind: Conversion::Bool, operand } => self.bit(operand),
            _ => {
                let value = self.value(expr);
                self.is_nonzero(value, span)
            }
        }
    }

    /// Whether a scalar is not zero, as one bit.
    fn is_nonzero(&mut self, value: Value, span: Span) -> Value {
        let ty = self.func[value].ty;
        if ty == Type::I1 {
            // Already the one bit, which is what a `bool` holds and what a comparison
            // produced. Comparing it against zero would answer the same question twice.
            return value;
        }
        let address = self.address;
        if ty.is_ptr() {
            let mut build = self.build(span);
            let zero = build.iconst(address, 0);
            let null = build.unary(Opcode::IntToPtr, zero, Type::PTR);
            return build.icmp(IntPred::Ne, value, null);
        }
        if ty.lane().is_float() {
            let mut build = self.build(span);
            let zero = build.fconst(ty, 0);
            return build.fcmp(FloatPred::Une, value, zero, Flags::NONE);
        }
        let mut build = self.build(span);
        let zero = build.iconst(ty, 0);
        build.icmp(IntPred::Ne, value, zero)
    }

    /// A constant the checking folded.
    fn constant(&mut self, value: Const, ty: TypeId, span: Span) -> Option<Value> {
        let ir = repr::value_type(self.types(), self.target(), ty)?;
        match value {
            Const::Int(number) if ir.is_ptr() => {
                // A null pointer constant, or a program that cast a number to a pointer.
                let address = self.address;
                let mut build = self.build(span);
                let number = build.iconst(address, number);
                Some(build.unary(Opcode::IntToPtr, number, Type::PTR))
            }
            Const::Int(number) => Some(self.build(span).iconst(ir, number)),
            Const::Float(number) => Some(self.build(span).fconst(ir, number.to_bits())),
            Const::Address(address) => {
                let symbol = match address.base {
                    rucc_sema::Base::Decl(decl) => self.unit.symbol_of(decl),
                    rucc_sema::Base::Str(id) => self.unit.string(id),
                };
                let addr = self.global_addr(symbol, span);
                Some(self.offset(addr, address.offset as u64, span))
            }
        }
    }

    /// A conversion the language performed.
    fn convert(
        &mut self,
        kind: Conversion,
        operand: ExprId,
        ty: TypeId,
        span: Span,
    ) -> Option<Value> {
        let from = self.tast()[operand].ty;
        match kind {
            Conversion::Lvalue => {
                let place = self.place(operand);
                self.read(place, span)
            }
            Conversion::ArrayDecay | Conversion::FunctionDecay => {
                let place = self.place(operand);
                Some(self.address_of(place, span))
            }
            Conversion::Arithmetic | Conversion::Pointer => {
                let value = self.eval(operand)?;
                Some(self.coerce(value, from, ty, span))
            }
            Conversion::Bool => Some(self.bit(operand)),
            Conversion::NullPointer => {
                self.eval(operand);
                let address = self.address;
                let mut build = self.build(span);
                let zero = build.iconst(address, 0);
                Some(build.unary(Opcode::IntToPtr, zero, Type::PTR))
            }
            Conversion::Void => {
                self.eval(operand);
                None
            }
        }
    }

    /// One scalar type to another, which is what a cast and an argument both do.
    fn coerce(&mut self, value: Value, from: TypeId, to: TypeId, span: Span) -> Value {
        let types = self.unit.types;
        let target = self.unit.target;
        let Some(into) = repr::value_type(types, target, to) else {
            self.unsupported("a conversion to this type", span);
            return value;
        };
        let out = self.func[value].ty;
        if out == into {
            return value;
        }
        let signed = repr::is_signed(types, target, from);
        // A conversion to `bool` is a comparison against zero and not a narrowing, which is
        // what makes `(bool)2` one and not zero.
        if into == Type::I1 {
            return self.is_nonzero(value, span);
        }
        match (out.is_ptr(), out.lane().is_float(), into.is_ptr(), into.lane().is_float()) {
            (true, _, true, _) => value,
            (true, _, false, false) => {
                let address = self.address;
                let number = self.build(span).unary(Opcode::PtrToInt, value, address);
                self.widen(number, false, into, span)
            }
            (false, false, true, _) => {
                let address = self.address;
                let number = self.widen(value, signed, address, span);
                self.build(span).unary(Opcode::IntToPtr, number, Type::PTR)
            }
            (false, false, false, false) => self.widen(value, signed, into, span),
            (false, false, false, true) => {
                let opcode = if signed { Opcode::SIToFP } else { Opcode::UIToFP };
                self.build(span).unary(opcode, value, into)
            }
            (false, true, false, false) => {
                let opcode = if repr::is_signed(types, target, to) {
                    Opcode::FPToSI
                } else {
                    Opcode::FPToUI
                };
                self.build(span).unary(opcode, value, into)
            }
            (false, true, false, true) => {
                let opcode = if into.bits() > out.bits() { Opcode::FPExt } else { Opcode::FPTrunc };
                self.build(span).unary(opcode, value, into)
            }
            _ => {
                self.unsupported("this conversion", span);
                value
            }
        }
    }

    /// A prefix or postfix operator.
    fn unary(&mut self, op: UnaryOp, operand: ExprId, ty: TypeId, span: Span) -> Option<Value> {
        match op {
            UnaryOp::Plus => self.eval(operand),
            UnaryOp::Minus => {
                let value = self.eval(operand)?;
                let out = self.func[value].ty;
                if out.lane().is_float() {
                    return Some(self.build(span).unary(Opcode::FNeg, value, out));
                }
                let signed = repr::is_signed(self.types(), self.target(), ty);
                let flags = if signed { Flags::NSW } else { Flags::NONE };
                let mut build = self.build(span);
                let zero = build.iconst(out, 0);
                Some(build.binary(Opcode::Sub, zero, value, flags))
            }
            UnaryOp::Not => {
                let bit = self.bit(operand);
                let mut build = self.build(span);
                let one = build.iconst(Type::I1, 1);
                let flipped = build.binary(Opcode::Xor, bit, one, Flags::NONE);
                let into = self.value_type(ty, span);
                Some(self.widen(flipped, false, into, span))
            }
            UnaryOp::BitNot => {
                let value = self.eval(operand)?;
                let out = self.func[value].ty;
                let mut build = self.build(span);
                let ones = build.iconst(out, -1);
                Some(build.binary(Opcode::Xor, value, ones, Flags::NONE))
            }
            UnaryOp::Deref => {
                let place = self.place_of_deref(operand, ty);
                self.read(place, span)
            }
            UnaryOp::AddrOf => {
                let place = self.place(operand);
                Some(self.address_of(place, span))
            }
            UnaryOp::PreInc | UnaryOp::PreDec | UnaryOp::PostInc | UnaryOp::PostDec => {
                self.step_by_one(op, operand, span)
            }
            UnaryOp::Real | UnaryOp::Imag => {
                self.unsupported("a complex type", span);
                let ty = repr::value_type(self.types(), self.target(), ty)?;
                Some(self.poison(ty, span))
            }
        }
    }

    /// The place `*p` names.
    fn place_of_deref(&mut self, operand: ExprId, ty: TypeId) -> Place {
        let addr = self.value(operand);
        Place { at: Where::Addr(addr), ty }
    }

    /// `++x`, `--x`, `x++` and `x--`, which are one read, one add and one write.
    fn step_by_one(&mut self, op: UnaryOp, operand: ExprId, span: Span) -> Option<Value> {
        let ty = self.tast()[operand].ty;
        let place = self.place(operand);
        let old = self.read(place, span)?;
        let up = matches!(op, UnaryOp::PreInc | UnaryOp::PostInc);
        let out = self.func[old].ty;

        let new = if out.is_ptr() {
            let pointee = match self.types().kind(self.types().canonical(ty)) {
                TypeKind::Pointer(pointee) => pointee,
                _ => ty,
            };
            let size = repr::size_of(self.types(), self.target(), pointee);
            let address = self.address;
            let one = self.build(span).iconst(address, 1);
            self.step(old, one, false, size, !up, span)
        } else if out.lane().is_float() {
            let format = repr::float_format_of(self.types(), self.target(), ty);
            let one = format.map_or(0, |format| Real::from_signed(1, format).0.to_bits());
            let mut build = self.build(span);
            let one = build.fconst(out, one);
            let opcode = if up { Opcode::FAdd } else { Opcode::FSub };
            build.binary(opcode, old, one, Flags::NONE)
        } else {
            let signed = repr::is_signed(self.types(), self.target(), ty);
            let flags = if signed { Flags::NSW } else { Flags::NONE };
            let mut build = self.build(span);
            let one = build.iconst(out, 1);
            let opcode = if up { Opcode::Add } else { Opcode::Sub };
            build.binary(opcode, old, one, flags)
        };
        self.write(place, new, span);
        Some(if op.is_postfix() { old } else { new })
    }

    /// A binary operator.
    fn binary(
        &mut self,
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
        ty: TypeId,
        span: Span,
    ) -> Option<Value> {
        match op {
            // Both of these answer with one bit, and C says the type of the answer is `int`.
            BinaryOp::LogAnd | BinaryOp::LogOr => {
                let bit = self.short_circuit(op, lhs, rhs, span);
                let into = self.value_type(ty, span);
                Some(self.widen(bit, false, into, span))
            }
            _ if op.is_comparison() => {
                let operand = self.tast()[lhs].ty;
                let left = self.value(lhs);
                let right = self.value(rhs);
                let bit = self.compare(op, left, right, operand, span);
                let into = self.value_type(ty, span);
                Some(self.widen(bit, false, into, span))
            }
            BinaryOp::Add | BinaryOp::Sub if self.is_pointer(ty) => {
                self.pointer_arithmetic(op, lhs, rhs, ty, span)
            }
            BinaryOp::Sub if self.is_pointer(self.tast()[lhs].ty) => {
                self.pointer_difference(lhs, rhs, ty, span)
            }
            _ => {
                let left = self.value(lhs);
                let right = self.value(rhs);
                Some(self.arithmetic(op, left, right, ty, span))
            }
        }
    }

    /// Whether a type is a pointer, which is what tells `+` which `+` it is.
    fn is_pointer(&self, ty: TypeId) -> bool {
        matches!(self.types().kind(self.types().canonical(ty)), TypeKind::Pointer(_))
    }

    /// The element type of a pointer type.
    fn pointee(&self, ty: TypeId) -> TypeId {
        match self.types().kind(self.types().canonical(ty)) {
            TypeKind::Pointer(pointee) => pointee,
            _ => ty,
        }
    }

    /// `p + n`, `n + p` and `p - n`.
    fn pointer_arithmetic(
        &mut self,
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
        ty: TypeId,
        span: Span,
    ) -> Option<Value> {
        let (pointer, steps) =
            if self.is_pointer(self.tast()[lhs].ty) { (lhs, rhs) } else { (rhs, lhs) };
        let index = self.tast()[steps].ty;
        let base = self.value(pointer);
        let amount = self.value(steps);
        let size = repr::size_of(self.types(), self.target(), self.pointee(ty));
        let signed = repr::is_signed(self.types(), self.target(), index);
        Some(self.step(base, amount, signed, size, op == BinaryOp::Sub, span))
    }

    /// `p - q`, which is how many elements apart they are.
    fn pointer_difference(
        &mut self,
        lhs: ExprId,
        rhs: ExprId,
        ty: TypeId,
        span: Span,
    ) -> Option<Value> {
        let pointee = self.pointee(self.tast()[lhs].ty);
        let size = repr::size_of(self.types(), self.target(), pointee).max(1);
        let left = self.value(lhs);
        let right = self.value(rhs);
        let address = self.address;
        let mut build = self.build(span);
        let left = build.unary(Opcode::PtrToInt, left, address);
        let right = build.unary(Opcode::PtrToInt, right, address);
        let bytes = build.binary(Opcode::Sub, left, right, Flags::NONE);
        let elements = if size == 1 {
            bytes
        } else {
            let scale = build.iconst(address, size as i128);
            build.binary(Opcode::SDiv, bytes, scale, Flags::EXACT)
        };
        let into = self.value_type(ty, span);
        Some(self.widen(elements, true, into, span))
    }

    /// An arithmetic or bitwise operator on two values of one type.
    fn arithmetic(
        &mut self,
        op: BinaryOp,
        lhs: Value,
        mut rhs: Value,
        ty: TypeId,
        span: Span,
    ) -> Value {
        let out = self.func[lhs].ty;
        let float = out.lane().is_float();
        let signed = repr::is_signed(self.types(), self.target(), ty);
        let shift = matches!(op, BinaryOp::Shl | BinaryOp::Shr);
        if shift {
            // The two sides of a shift are promoted apart, so the count arrives in whatever
            // type it was written in and the IR wants both operands alike.
            rhs = self.widen(rhs, false, out, span);
        }
        let opcode = match (op, float, signed) {
            (BinaryOp::Mul, true, _) => Opcode::FMul,
            (BinaryOp::Div, true, _) => Opcode::FDiv,
            (BinaryOp::Rem, true, _) => Opcode::FRem,
            (BinaryOp::Add, true, _) => Opcode::FAdd,
            (BinaryOp::Sub, true, _) => Opcode::FSub,
            (BinaryOp::Mul, false, _) => Opcode::Mul,
            (BinaryOp::Div, false, true) => Opcode::SDiv,
            (BinaryOp::Div, false, false) => Opcode::UDiv,
            (BinaryOp::Rem, false, true) => Opcode::SRem,
            (BinaryOp::Rem, false, false) => Opcode::URem,
            (BinaryOp::Add, false, _) => Opcode::Add,
            (BinaryOp::Sub, false, _) => Opcode::Sub,
            (BinaryOp::Shl, _, _) => Opcode::Shl,
            (BinaryOp::Shr, _, true) => Opcode::AShr,
            (BinaryOp::Shr, _, false) => Opcode::LShr,
            (BinaryOp::BitAnd, _, _) => Opcode::And,
            (BinaryOp::BitXor, _, _) => Opcode::Xor,
            (BinaryOp::BitOr, _, _) => Opcode::Or,
            _ => {
                self.unsupported("this operator", span);
                return lhs;
            }
        };
        // Signed overflow is undefined, so the arithmetic may be assumed not to overflow, and
        // that is what lets a comparison of `i + 1` with `n` be folded. `-fwrapv` is what takes
        // the assumption away, and it is not wired up yet.
        let flags = match (opcode, signed) {
            (Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::Shl, true) => Flags::NSW,
            _ => Flags::NONE,
        };
        self.build(span).binary(opcode, lhs, rhs, flags)
    }

    /// A comparison, whose answer is one bit.
    fn compare(
        &mut self,
        op: BinaryOp,
        lhs: Value,
        rhs: Value,
        operand: TypeId,
        span: Span,
    ) -> Value {
        if self.func[lhs].ty.lane().is_float() {
            let pred = match op {
                BinaryOp::Lt => FloatPred::Olt,
                BinaryOp::Gt => FloatPred::Ogt,
                BinaryOp::Le => FloatPred::Ole,
                BinaryOp::Ge => FloatPred::Oge,
                BinaryOp::Eq => FloatPred::Oeq,
                // Not equal is true when the two are unordered, which is what makes
                // `x != x` a test for a NaN.
                _ => FloatPred::Une,
            };
            return self.build(span).fcmp(pred, lhs, rhs, Flags::NONE);
        }
        let signed = repr::is_signed(self.types(), self.target(), operand);
        let pred = match (op, signed) {
            (BinaryOp::Lt, true) => IntPred::Slt,
            (BinaryOp::Lt, false) => IntPred::Ult,
            (BinaryOp::Gt, true) => IntPred::Sgt,
            (BinaryOp::Gt, false) => IntPred::Ugt,
            (BinaryOp::Le, true) => IntPred::Sle,
            (BinaryOp::Le, false) => IntPred::Ule,
            (BinaryOp::Ge, true) => IntPred::Sge,
            (BinaryOp::Ge, false) => IntPred::Uge,
            (BinaryOp::Eq, _) => IntPred::Eq,
            _ => IntPred::Ne,
        };
        self.build(span).icmp(pred, lhs, rhs)
    }

    /// `a && b` and `a || b`, whose right side is evaluated only when it decides the answer.
    fn short_circuit(&mut self, op: BinaryOp, lhs: ExprId, rhs: ExprId, span: Span) -> Value {
        let and = op == BinaryOp::LogAnd;
        let left = self.condition(lhs);
        let var = self.temp();
        let block = self.block();
        // The answer if the right side is never evaluated, which is the left side's own value.
        let shortcut = self.build(span).iconst(Type::I1, i128::from(!and));
        self.ssa.write(var, block, shortcut);

        let other = self.new_block();
        let join = self.new_block();
        if and {
            self.br_if(left, other, join, span);
        } else {
            self.br_if(left, join, other, span);
        }
        self.ssa.seal(self.func, other);

        self.at = Some(other);
        let right = self.condition(rhs);
        let block = self.block();
        self.ssa.write(var, block, right);
        self.jump(join, span);

        self.ssa.seal(self.func, join);
        self.at = Some(join);
        self.ssa.read(self.func, var, join, Type::I1)
    }

    /// `cond ? then : otherwise`.
    fn conditional(
        &mut self,
        cond: ExprId,
        then: ExprId,
        otherwise: ExprId,
        ty: TypeId,
        span: Span,
    ) -> Option<Value> {
        let into = repr::value_type(self.types(), self.target(), ty);
        let value = self.condition(cond);
        let then_block = self.new_block();
        let else_block = self.new_block();
        self.br_if(value, then_block, else_block, span);
        self.ssa.seal(self.func, then_block);
        self.ssa.seal(self.func, else_block);

        let var = self.temp();
        let mut join = None;
        for (block, arm) in [(then_block, then), (else_block, otherwise)] {
            self.at = Some(block);
            let value = self.eval(arm);
            if self.at.is_none() {
                continue;
            }
            if let (Some(_), Some(value)) = (into, value) {
                let at = self.block();
                self.ssa.write(var, at, value);
            }
            let target = match join {
                Some(block) => block,
                None => {
                    let block = self.new_block();
                    join = Some(block);
                    block
                }
            };
            self.jump(target, span);
        }

        self.at = join;
        let join = join?;
        self.ssa.seal(self.func, join);
        let into = into?;
        Some(self.ssa.read(self.func, var, join, into))
    }

    /// An assignment, plain or compound.
    fn assign(
        &mut self,
        op: Option<BinaryOp>,
        computation: TypeId,
        lhs: ExprId,
        rhs: ExprId,
        span: Span,
    ) -> Option<Value> {
        let ty = self.tast()[lhs].ty;
        let place = self.place(lhs);
        let Some(op) = op else {
            if repr::value_type(self.types(), self.target(), ty).is_none() {
                return self.copy(place, rhs, ty, span);
            }
            let value = self.eval(rhs)?;
            self.write(place, value, span);
            return Some(value);
        };

        // `a op= b` is not `a = a op b` with the conversions left out: the operation happens in
        // the computation type and the answer is converted back, which is why `i /= 0.5` on an
        // `int` divides in `double`.
        let old = self.read(place, span)?;
        let old = self.coerce(old, ty, computation, span);
        let value = if self.is_pointer(computation) {
            let steps = self.value(rhs);
            let index = self.tast()[rhs].ty;
            let size = repr::size_of(self.types(), self.target(), self.pointee(computation));
            let signed = repr::is_signed(self.types(), self.target(), index);
            self.step(old, steps, signed, size, op == BinaryOp::Sub, span)
        } else {
            let right = self.value(rhs);
            self.arithmetic(op, old, right, computation, span)
        };
        let value = self.coerce(value, computation, ty, span);
        self.write(place, value, span);
        Some(value)
    }

    /// `a = b` where the two are structures, which is a copy and not a value.
    fn copy(&mut self, place: Place, rhs: ExprId, ty: TypeId, span: Span) -> Option<Value> {
        let source = self.place(rhs);
        let source = self.address_of(source, span);
        let destination = self.address_of(place, span);
        let size = repr::size_of(self.types(), self.target(), ty);
        let align = repr::align_of(self.types(), self.target(), ty);
        self.memcpy(destination, source, size, align, span);
        None
    }

    /// A call, direct when the callee is a function and indirect when it is a pointer.
    fn call(&mut self, callee: ExprId, args: rucc_sema::ExprList, span: Span) -> Option<Value> {
        let tast = self.tast();
        let ty = tast[callee].ty;
        let signature = self.unit.signature(ty, span)?;

        let mut values = Vec::with_capacity(tast[args].len());
        for index in 0..tast[args].len() {
            let arg = tast[args][index];
            values.push(self.value(arg));
        }

        let direct = self.direct(callee);
        let returns = signature.returns.len();
        let inst = match direct {
            Some(symbol) => {
                let sig = self.func.add_signature(signature);
                self.build(span).call(symbol, sig, &values)
            }
            None => {
                let addr = self.value(callee);
                let mut build = self.build(span);
                let sig = build.func().add_signature(signature);
                let info = build.func().add_call(CallInfo { callee: None, signature: sig });
                let returns = build.func()[sig].returns.clone();
                let mut operands = Vec::with_capacity(values.len() + 1);
                operands.push(addr);
                operands.extend_from_slice(&values);
                let args = build.func().push_values(&operands);
                build.inst(
                    InstData {
                        args,
                        extra: Extra::Call(info),
                        ..InstData::new(Opcode::CallIndirect)
                    },
                    &returns,
                )
            }
        };
        if returns == 0 {
            return None;
        }
        self.func[inst].results().next()
    }

    /// The name a call goes to, when it goes to one rather than through a pointer.
    fn direct(&mut self, callee: ExprId) -> Option<rucc_base::Symbol> {
        let tast = self.tast();
        let ExprKind::Convert { kind: Conversion::FunctionDecay, operand } = tast[callee].kind
        else {
            return None;
        };
        let ExprKind::Decl(decl) = tast[operand].kind else { return None };
        Some(self.unit.symbol_of(decl))
    }

    /// Reports a construct the walk does not build IR for yet.
    fn unsupported(&mut self, what: &str, span: Span) {
        self.unit.unsupported(what, span);
    }
}

/// Whether a statement holds a label anywhere inside it that something outside it can reach.
///
/// Asked of a statement in unreachable code, because dropping one with a label in it drops a
/// place a `switch` or a `goto` branches to, and what the branch would then point at is a block
/// with nothing in it. `cases` says whether a `case` or a `default` counts, and it stops
/// counting inside a nested `switch`, since those labels belong to that `switch` and go away
/// with it. A `goto` label is looked for everywhere, because a `goto` can be anywhere in the
/// function.
fn holds_a_label(tast: &Tast, id: StmtId, cases: bool) -> bool {
    match tast[id] {
        Stmt::Label { .. } => true,
        Stmt::Case { body, .. } | Stmt::Default { body } => {
            cases || holds_a_label(tast, body, cases)
        }
        Stmt::Block(list) => {
            (0..tast[list].len()).any(|index| holds_a_label(tast, tast[list][index], cases))
        }
        Stmt::If { then, otherwise, .. } => {
            holds_a_label(tast, then, cases)
                || otherwise.is_some_and(|id| holds_a_label(tast, id, cases))
        }
        Stmt::While { body, .. } | Stmt::DoWhile { body, .. } | Stmt::For { body, .. } => {
            holds_a_label(tast, body, cases)
        }
        Stmt::Switch { body, .. } => holds_a_label(tast, body, false),
        _ => false,
    }
}

/// The pass that decides what the function needs before any of it is walked.
///
/// Two questions, and both have to be answered for the whole body at once. Which locals need a
/// stack slot, because an `alloca` belongs in the entry block and the walk meets `&x` long
/// after it has left. And which declarations inside the body are really globals, because a
/// `static` in a function is emitted at the module level and a reference to it is a reference
/// to a name.
struct Scan<'a> {
    tast: &'a Tast,
    /// The declarations something takes the address of.
    escaped: HashSet<DeclId>,
    /// Every object with automatic storage the body declares, in the order it declares them.
    locals: Vec<DeclId>,
    /// Every object with static storage the body declares.
    statics: Vec<DeclId>,
}

impl Scan<'_> {
    /// One statement and everything under it.
    fn stmt(&mut self, id: StmtId) {
        match self.tast[id] {
            Stmt::Error | Stmt::Empty | Stmt::Break | Stmt::Continue | Stmt::Goto(_) => {}
            Stmt::Expr(expr) => self.expr(expr),
            Stmt::IndirectGoto(expr) => self.expr(expr),
            Stmt::Block(list) => {
                for index in 0..self.tast[list].len() {
                    let stmt = self.tast[list][index];
                    self.stmt(stmt);
                }
            }
            Stmt::Decls(list) => {
                for index in 0..self.tast[list].len() {
                    let decl = self.tast[list][index];
                    self.decl(decl);
                }
            }
            Stmt::If { cond, then, otherwise } => {
                self.expr(cond);
                self.stmt(then);
                if let Some(otherwise) = otherwise {
                    self.stmt(otherwise);
                }
            }
            Stmt::While { cond, body } | Stmt::DoWhile { body, cond } => {
                self.expr(cond);
                self.stmt(body);
            }
            Stmt::For { init, cond, step, body } => {
                if let Some(init) = init {
                    self.stmt(init);
                }
                if let Some(cond) = cond {
                    self.expr(cond);
                }
                if let Some(step) = step {
                    self.expr(step);
                }
                self.stmt(body);
            }
            Stmt::Switch { cond, body, .. } => {
                self.expr(cond);
                self.stmt(body);
            }
            Stmt::Case { body, .. } | Stmt::Default { body } | Stmt::Label { body, .. } => {
                self.stmt(body);
            }
            Stmt::Return(value) => {
                if let Some(value) = value {
                    self.expr(value);
                }
            }
        }
    }

    /// One declaration, and the initializer it has.
    fn decl(&mut self, id: DeclId) {
        if self.tast[id].duration == StorageDuration::Automatic {
            self.locals.push(id);
        } else {
            self.statics.push(id);
        }
        if let Some(init) = self.tast[id].init {
            for index in 0..self.tast[init].len() {
                let entry = self.tast[init][index];
                self.expr(entry.value);
            }
        }
    }

    /// One expression and everything under it.
    fn expr(&mut self, id: ExprId) {
        match self.tast[id].kind {
            ExprKind::Error | ExprKind::Const(_) | ExprKind::Str(_) | ExprKind::Decl(_) => {}
            ExprKind::LabelAddr(_) => {}
            ExprKind::Member { base, .. } => self.expr(base),
            ExprKind::Subscript { base, index } => {
                self.expr(base);
                self.expr(index);
            }
            ExprKind::Call { callee, args } => {
                self.expr(callee);
                for index in 0..self.tast[args].len() {
                    let arg = self.tast[args][index];
                    self.expr(arg);
                }
            }
            ExprKind::Unary { op: UnaryOp::AddrOf, operand } => {
                self.escape(operand);
                self.expr(operand);
            }
            ExprKind::Unary { operand, .. } => self.expr(operand),
            ExprKind::Binary { lhs, rhs, .. } | ExprKind::Comma { lhs, rhs } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Assign { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Cond { cond, then, otherwise } => {
                self.expr(cond);
                self.expr(then);
                self.expr(otherwise);
            }
            ExprKind::Cast(operand) | ExprKind::Convert { operand, .. } => self.expr(operand),
            ExprKind::CompoundLiteral(decl) => self.decl(decl),
            ExprKind::StmtExpr(body) => self.stmt(body),
            ExprKind::VaArg { list } => {
                self.escape(list);
                self.expr(list);
            }
        }
    }

    /// Marks the object an address was taken of, if it was taken of one.
    fn escape(&mut self, id: ExprId) {
        match self.tast[id].kind {
            ExprKind::Decl(decl) | ExprKind::CompoundLiteral(decl) => {
                self.escaped.insert(decl);
            }
            // `&s.field` is an address into `s`, so it is `s` that needs one. A subscript is
            // not here on purpose: its base is a pointer and the object it points at is
            // wherever that pointer came from.
            ExprKind::Member { base, .. } => self.escape(base),
            _ => {}
        }
    }
}
