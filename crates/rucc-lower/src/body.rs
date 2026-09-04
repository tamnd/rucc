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

use rucc_ast::{AsmQuals, BinaryOp, UnaryOp};
use rucc_base::float::Float as Real;
use rucc_diag::Span;
use rucc_ir::{
    AsmInfo, Block, BlockCall, Builder, CallInfo, Extra, Flags, FloatPred, Func, Inst, InstData,
    IntPred, MemInfo, MemOrder, Opcode, Type, Value, ValueList,
};
use rucc_sema::{
    Classify, Const, Conversion, DeclId, ExprId, ExprKind, InitEntry, Sign, Stmt, StmtId,
    StorageDuration, Tast,
};
use rucc_target::{Pass, TargetInfo};
use rucc_types::{ArrayLen, Qualifiers, TypeId, TypeKind, Types, VlaId};

use crate::abi::{Plan, Travel};
use crate::bits::{Piece, Run};
use crate::repr;
use crate::ssa::{Ssa, Var};
use crate::unit::Unit;

/// Builds the body of one function definition into `func`.
///
/// The plan is how the call travels, which is what says the entry block's parameters: one per
/// C parameter for the ones that travel as themselves, several for one taken apart into
/// registers, none at all for one with no bytes in it, and a hidden first one when the return
/// value is written through a pointer the caller passes.
pub(crate) fn lower(unit: &mut Unit<'_>, decl: DeclId, func: &mut Func, plan: &Plan) {
    let tast = unit.tast;
    let Some(root) = tast[decl].body else { return };
    let params = tast[decl].params;
    let span = tast.decl_span(decl);
    if tast[params].len() != plan.args.len() {
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
        taken: Vec::new(),
        loops: Vec::new(),
        next_var: 0,
        address,
        ret: plan.ret.clone(),
        sret: None,
        vlas: HashMap::new(),
        marks: Vec::new(),
        next_scope: 0,
        landings: HashMap::new(),
        jumps: Vec::new(),
        grows: false,
    };
    body.ssa.seal(body.func, entry);

    // What the whole function needs decided before any of it is walked.
    let mut scan = Scan {
        tast,
        escaped: HashSet::new(),
        locals: Vec::new(),
        statics: Vec::new(),
        taken: Vec::new(),
    };
    scan.stmt(root);
    let Scan { escaped, locals, statics, taken, .. } = scan;
    // A label whose address is taken and which is never defined was reported by the checking,
    // and there is no block for one, so it is not somewhere a jump can arrive.
    body.taken = taken.iter().filter_map(|&label| tast[label].stmt).collect();
    for decl in statics {
        body.unit.local_static(decl);
    }

    // The slots first, so that every `alloca` is at the top of the entry block, and then the
    // parameters, whose stores have to come after the slots they store into.
    let params = tast[params].to_vec();
    // Whether anything in the function grows the stack, which decides what a `goto` can do.
    let declared: Vec<TypeId> = params.iter().chain(locals.iter()).map(|&d| tast[d].ty).collect();
    body.grows = declared.iter().any(|&ty| repr::is_variable_length(body.types(), ty));
    for &param in &params {
        body.declare(param, escaped.contains(&param));
    }
    for &local in &locals {
        body.declare(local, escaped.contains(&local));
    }
    // The address the return value is written to, which is the first thing the caller passes
    // and therefore the first parameter, before anything the program wrote.
    if plan.returns_through_memory() {
        body.sret = Some(body.func.append_param(entry, Type::PTR));
    }
    for (index, &param) in params.iter().enumerate() {
        let Some(travel) = plan.args.get(index) else { continue };
        body.parameter(entry, param, travel, span);
    }

    // A parameter can be declared with a variably modified type, `void f(int n, int a[][n])`,
    // and the size in it is evaluated where the declaration is, which for a parameter is here.
    // Everything the body does with `a` reads the value taken now and not `n` as it is then.
    for &param in &params {
        let ty = tast[param].ty;
        body.measure(ty);
    }

    body.stmt(root);
    body.settle();
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

/// What a jump made in `from` has to do to the stack to land where a label in `to` is.
///
/// The two are paths of scopes from the top of the function down, so the scopes they share are
/// the ones both are inside and the rest are the ones the jump leaves. Restoring the oldest
/// stack pointer among those gives back everything the newer ones took as well, which is why one
/// restore is enough however many scopes are left at once.
///
/// A scope is shared only when it is the same scope and grew the stack at the same point. That
/// second half is what makes `L: int a[n]; goto L;` give the array back: the label was passed
/// before the array was made, so it is a place where the array does not exist, and jumping there
/// leaves its scope even though the block is the same one.
fn landing(from: &[Mark], to: &[Mark]) -> Landing {
    if to.iter().enumerate().any(|(at, mark)| mark.saved.is_some() && from.get(at) != Some(mark)) {
        return Landing::Enters;
    }
    let leaving = from.iter().enumerate().find_map(|(at, mark)| {
        let saved = mark.saved?;
        (to.get(at) != Some(mark)).then_some(saved)
    });
    match leaving {
        Some(saved) => Landing::Restore(saved),
        None => Landing::Same,
    }
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
        let insts: Vec<Inst> = func.insts(block).collect();
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

/// The three kinds of place there are.
#[derive(Debug, Clone, Copy)]
enum Where {
    /// A variable with no address, which a load and a store are a read and a write of.
    Var(Var),
    /// An address, which a load and a store are a load and a store of.
    Addr(Value),
    /// A run of bits after an address, which is a bit-field. A read of one is a load and a
    /// shift and a write is a load, a mask and a store, both of which [`crate::bits`] says
    /// the shape of.
    Bits(Value, Run),
}

/// How far one step over a type moves, which is a number of bytes for every type but a
/// variably modified one, whose is a value the walk worked out where the declaration was.
#[derive(Debug, Clone, Copy)]
enum Stride {
    /// So many bytes, which is what `sizeof` answers with.
    Bytes(u64),
    /// This many, which is what the sizes of a variable length array multiplied out to.
    Value(Value),
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
    /// How many scopes were open when it was pushed, which is what a `break` or a `continue`
    /// leaving it has to give the stack back down to.
    depth: usize,
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
    /// The labelled statements the function takes the address of, in the order it takes them,
    /// which is where a `goto *p` can arrive. Collected before the walk starts, since the
    /// address of a label can be taken after the jump that uses it.
    taken: Vec<StmtId>,
    loops: Vec<Frame>,
    next_var: u32,
    /// The integer type an address is as wide as.
    address: Type,
    /// How the return value comes back, which every `return` in the function has to build.
    ret: Travel,
    /// The address the return value is written to, for a function that returns through one.
    sret: Option<Value>,
    /// What each variable length array met so far is long, keyed by the expression it was
    /// written as. C says that expression is evaluated where the declaration having it is
    /// reached and not again, so `int a[n]; n = 0;` leaves `sizeof a` what it was.
    vlas: HashMap<ExprId, Value>,
    /// One entry per open scope, outermost first.
    marks: Vec<Mark>,
    /// How many scopes have been opened, which is what gives the next one a name of its own.
    next_scope: u32,
    /// What each label the walk has reached is inside, which is what a jump to it has to put the
    /// stack back to. Only collected for a function that grows the stack, since nothing else
    /// asks.
    landings: HashMap<StmtId, Vec<Mark>>,
    /// The jumps whose stack is not settled yet, which is all of them until the walk knows where
    /// every label is.
    jumps: Vec<Jump>,
    /// Whether anything the function declares is an array whose length is not a constant, which
    /// is what makes the stack move under it.
    grows: bool,
}

/// One open scope, and the stack pointer as it was before anything in it grew the stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mark {
    /// What tells this scope apart from every other one. Two scopes that have grown nothing look
    /// alike otherwise, and a jump has to know which of them it is leaving.
    scope: u32,
    /// The stack pointer saved on the way through it, absent until something in it grows the
    /// stack and absent for good in the scopes that never do.
    saved: Option<Value>,
}

/// A jump whose stack cannot be settled where it is built.
///
/// A `goto` is allowed to name a label the walk has not reached yet, so what the stack should be
/// on arrival is not known there. What is left behind is the branch and the scopes the jump was
/// made in, and [`Body::settle`] puts the restore in front of the branch once every label has
/// been reached.
#[derive(Debug)]
struct Jump {
    /// The branch, which the restore goes in front of.
    inst: Inst,
    /// The scopes the jump was made in.
    from: Vec<Mark>,
    /// The labelled statements control can arrive at, which is one for a `goto` and every label
    /// whose address the function takes for a computed one.
    targets: Vec<StmtId>,
    /// What to call it in the message, for the shapes that are still turned down.
    what: &'static str,
    /// Where it was written.
    span: Span,
}

/// What a jump has to do to the stack to land where a label is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Landing {
    /// Nothing. Everything the label expects to be on the stack is already there.
    Same,
    /// Put the stack pointer back to this, which gives back every scope the jump leaves. The
    /// oldest of them is enough, since restoring it takes back what the newer ones did too.
    Restore(Value),
    /// The label is inside a scope that grew the stack and the jump is not in that scope, so
    /// arriving there means arriving somewhere an object was never made. C does not allow it,
    /// and the checking turns a `goto` that does it down before the walk runs, so what is left
    /// here is a computed one.
    Enters,
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
    fn jump(&mut self, target: Block, span: Span) -> Inst {
        let inst = self.build(span).jump(target, &[]);
        self.ssa.branch(self.func, inst);
        self.at = None;
        inst
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
            // Nothing here: an object whose size is not known until the walk reaches the
            // declaration cannot have its slot made in advance, so it is made there.
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

    /// A stack slot in the entry block, wherever the walk has got to.
    ///
    /// The scratch a call needs is not known before the walk reaches the call, and an `alloca`
    /// of a fixed size belongs at the top of the function however late it was decided on: one in
    /// a loop is a stack that grows every time round. So it is built detached and put in front
    /// of whatever the entry block starts with.
    fn scratch(&mut self, size: u64, align: u32, span: Span) -> Value {
        let entry = self.func.entry().expect("a body being walked has an entry block");
        let info = MemInfo { size, align, order: MemOrder::NotAtomic, tbaa: None };
        let mem = self.func.add_mem(info);
        let data = InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) };
        let first = self.func.insts(entry).next();
        match first {
            Some(first) => {
                let inst = self.func.create_inst(data, &[Type::PTR], span);
                self.func.insert_before(inst, first);
                self.func[inst].results().next().expect("an alloca produces its address")
            }
            None => Builder::new(self.func, entry).at(span).value(data, Type::PTR),
        }
    }

    /// A stack slot whose size is not known until the walk gets there, which is what an object
    /// of a variably modified type lives in.
    ///
    /// It is where the declaration is rather than in the entry block, because that is where the
    /// size is known and because C says the object comes into existence there. The stack it
    /// takes is given back at the end of the scope it was declared in.
    fn dynamic(&mut self, size: Value, align: u32, span: Span) -> Value {
        self.mark(span);
        let info = MemInfo { size: 0, align, order: MemOrder::NotAtomic, tbaa: None };
        let mut build = self.build(span);
        let mem = build.func().add_mem(info);
        let args = build.func().push_values(&[size]);
        build.value(
            InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) },
            Type::PTR,
        )
    }

    /// Saves the stack pointer for the scope the walk is in, if it has not been saved already.
    ///
    /// The save is where the first thing that grows the stack is rather than at the top of the
    /// scope, which is the same pointer and one instruction fewer in a scope that turns out to
    /// grow nothing. A scope that grows the stack twice saves once and gives both back together.
    fn mark(&mut self, span: Span) {
        let Some(scope) = self.marks.last().copied() else { return };
        if scope.saved.is_some() {
            return;
        }
        let saved = self.build(span).value(InstData::new(Opcode::StackSave), Type::PTR);
        if let Some(last) = self.marks.last_mut() {
            last.saved = Some(saved);
        }
    }

    /// Opens a scope, which is a block of statements the stack is given back at the end of.
    fn open(&mut self) {
        let scope = self.next_scope;
        self.next_scope += 1;
        self.marks.push(Mark { scope, saved: None });
    }

    /// Closes the innermost scope, giving back what it grew the stack by.
    fn close(&mut self, span: Span) {
        let mark = self.marks.pop().expect("a scope is closed by whoever opened it");
        self.restore(mark.saved, span);
    }

    /// Gives the stack back down to what it was at `depth` scopes, for a `break` or a
    /// `continue` that leaves several scopes at once.
    ///
    /// The outermost of the marks being left is the one to restore, since it is the oldest
    /// stack pointer of them and restoring it takes back everything the inner ones did too.
    fn unwind(&mut self, depth: usize, span: Span) {
        let saved =
            self.marks.get(depth..).and_then(|open| open.iter().find_map(|mark| mark.saved));
        self.restore(saved, span);
    }

    /// One `stackrestore`, if there is a pointer to restore and somewhere to put it.
    fn restore(&mut self, saved: Option<Value>, span: Span) {
        let (Some(saved), Some(_)) = (saved, self.at) else { return };
        let mut build = self.build(span);
        let args = build.func().push_values(&[saved]);
        build.inst(InstData { args, ..InstData::new(Opcode::StackRestore) }, &[]);
    }

    /// One `stackrestore` in front of a branch, which is where a jump out of a scope gives back
    /// what that scope grew the stack by.
    fn restore_before(&mut self, saved: Value, branch: Inst, span: Span) {
        let args = self.func.push_values(&[saved]);
        let data = InstData { args, ..InstData::new(Opcode::StackRestore) };
        let inst = self.func.create_inst(data, &[], span);
        self.func.insert_before(inst, branch);
    }

    /// The object of a declaration whose type is variably modified, built where the walk
    /// reaches it.
    fn variable_length(&mut self, decl: DeclId) {
        let tast = self.tast();
        let ty = tast[decl].ty;
        let span = tast.decl_span(decl);
        // The sizes first, and once: they are what the object is as long as, and what every
        // `sizeof` of it and every step over its rows answers with afterwards.
        self.measure(ty);
        if !repr::is_variable_length(self.types(), ty) {
            // A declaration of a variably modified type that is not an array itself, `int
            // (*p)[n]`, whose object is an ordinary pointer with its slot already made. The
            // sizes in it still had to be evaluated here, which is what the measuring above is.
            return;
        }
        if tast[decl].duration != StorageDuration::Automatic || self.at.is_none() {
            // A variably modified object with static storage is reported by the checking, since
            // there is no run time at file scope to work its size out in.
            return;
        }
        let size = self.size_value(ty, span);
        let align =
            tast[decl].alignment.unwrap_or_else(|| repr::align_of(self.types(), self.target(), ty));
        let slot = self.dynamic(size, align, span);
        self.vars.insert(decl, Local::Slot(slot));
    }

    /// Evaluates the sizes in a type, where the declaration carrying it was reached.
    ///
    /// A type is a tree and the sizes in it are the leaves: `int (*p)[n][m]` has two of them,
    /// and both are evaluated here even though nothing has asked what `p` points at yet. Doing
    /// it any later would be reading `n` at the wrong time, which is the whole point of the
    /// rule that says the size of a variable length array is worked out where its declaration
    /// is and not where it is used.
    fn measure(&mut self, ty: TypeId) {
        let canonical = self.types().canonical(ty);
        match self.types().kind(canonical) {
            TypeKind::Pointer(pointee) => self.measure(pointee),
            TypeKind::Array { elem, len } => {
                if let ArrayLen::Variable(vla) = len {
                    self.count(vla);
                }
                self.measure(elem);
            }
            _ => {}
        }
    }

    /// How many elements one variable length array has, evaluated once and remembered.
    fn count(&mut self, vla: VlaId) -> Value {
        let expr = self.tast().vla_size(vla);
        if let Some(&value) = self.vlas.get(&expr) {
            return value;
        }
        let value = self.value(expr);
        self.vlas.insert(expr, value);
        value
    }

    /// How many bytes an object of this type is, as a value.
    ///
    /// A constant for every type but a variably modified one, which is a multiplication of what
    /// its sizes turned out to be by what its element is.
    fn size_value(&mut self, ty: TypeId, span: Span) -> Value {
        let address = self.address;
        let canonical = self.types().canonical(ty);
        let TypeKind::Array { elem, len } = self.types().kind(canonical) else {
            let size = repr::size_of(self.types(), self.target(), ty);
            return self.build(span).iconst(address, i128::from(size));
        };
        let count = match len {
            ArrayLen::Variable(vla) => {
                let value = self.count(vla);
                let ty = self.tast()[self.tast().vla_size(vla)].ty;
                let signed = repr::is_signed(self.types(), self.target(), ty);
                self.widen(value, signed, address, span)
            }
            ArrayLen::Fixed(count) => self.build(span).iconst(address, i128::from(count)),
            // An array of an unknown length has no size, and one of these is only reached
            // through a type the checking would have turned down.
            ArrayLen::Unknown | ArrayLen::Star => self.build(span).iconst(address, 0),
        };
        let elem = self.size_value(elem, span);
        self.build(span).binary(Opcode::Mul, count, elem, Flags::NSW)
    }

    /// How far one step over a type moves.
    fn stride(&mut self, ty: TypeId, span: Span) -> Stride {
        if repr::is_variable_length(self.types(), ty) {
            return Stride::Value(self.size_value(ty, span));
        }
        Stride::Bytes(repr::size_of(self.types(), self.target(), ty))
    }

    /// One of the function's own parameters, in whatever form the call brought it.
    fn parameter(&mut self, entry: Block, decl: DeclId, travel: &Travel, span: Span) {
        let ty = self.tast()[decl].ty;
        let local = self.vars.get(&decl).copied();
        match travel.pass {
            // An object with no bytes in it, which travels nowhere and has nothing to store.
            Pass::Ignore => {}
            Pass::Direct => {
                let value = self.func.append_param(entry, travel.types[0]);
                match local {
                    Some(Local::Value(var)) => self.ssa.write(var, entry, value),
                    Some(Local::Slot(slot)) => {
                        let info = self.access(ty);
                        self.build(span).store(value, slot, info, Flags::NONE);
                    }
                    None => {}
                }
            }
            Pass::Pieces(_) => {
                let types = travel.types.clone();
                let values: Vec<Value> =
                    types.iter().map(|ty| self.func.append_param(entry, *ty)).collect();
                if let Some(Local::Slot(slot)) = local {
                    self.store_slots(slot, travel, &values, span);
                }
            }
            // The caller passed the address of a copy, or of the bytes it put in the argument
            // area. Either way the object the body works on is the parameter's own slot, so
            // what arrives is copied into it and nothing else in the walk has to know.
            Pass::Reference | Pass::Memory => {
                let addr = self.func.append_param(entry, Type::PTR);
                if let Some(Local::Slot(slot)) = local {
                    self.memcpy(slot, addr, travel.size, travel.align, span);
                }
            }
        }
    }

    /// Writes the registers an aggregate travelled in into the object.
    fn store_slots(&mut self, addr: Value, travel: &Travel, values: &[Value], span: Span) {
        // A register holding the last few bytes of an object is as wide as a register and not as
        // wide as what is left, so storing it straight into the object would write past the end
        // of it. What that takes is a buffer wide enough for the registers, which the object is
        // then copied out of.
        let reach = travel.reach();
        let wide = reach > travel.size;
        let into = if wide { self.scratch(reach, travel.align, span) } else { addr };
        let slots: Vec<rucc_target::Slot> = travel.slots().to_vec();
        for (slot, value) in slots.iter().zip(values) {
            let at = self.offset(into, slot.offset(), span);
            let info = self.piece_info(travel.align, slot.offset());
            self.build(span).store(*value, at, info, Flags::NONE);
        }
        if wide {
            self.memcpy(addr, into, travel.size, travel.align, span);
        }
    }

    /// Reads the object into the registers it travels in.
    fn load_slots(&mut self, addr: Value, travel: &Travel, span: Span) -> Vec<Value> {
        let reach = travel.reach();
        let from = if reach > travel.size {
            // The same three bytes past the end of a five byte object, read this time.
            let buffer = self.scratch(reach, travel.align, span);
            self.memcpy(buffer, addr, travel.size, travel.align, span);
            buffer
        } else {
            addr
        };
        let slots: Vec<rucc_target::Slot> = travel.slots().to_vec();
        let types = travel.types.clone();
        let mut values = Vec::with_capacity(slots.len());
        for (slot, ty) in slots.iter().zip(types) {
            let at = self.offset(from, slot.offset(), span);
            let info = self.piece_info(travel.align, slot.offset());
            values.push(self.build(span).load(ty, at, info, Flags::NONE));
        }
        values
    }

    /// How aligned one register's worth of an object is, which is what its offset leaves of the
    /// object's own alignment.
    fn piece_info(&self, align: u32, offset: u64) -> MemInfo {
        let at = if offset == 0 { align } else { align.min(1 << offset.trailing_zeros().min(16)) };
        MemInfo { size: 0, align: at.max(1), order: MemOrder::NotAtomic, tbaa: None }
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
            Stmt::Expr(expr) => self.discard(expr),
            Stmt::Block(list) => {
                self.open();
                for index in 0..tast[list].len() {
                    let stmt = tast[list][index];
                    self.stmt(stmt);
                }
                self.close(span);
            }
            Stmt::Decls(list) => {
                for index in 0..tast[list].len() {
                    let decl = tast[list][index];
                    // A declaration of a variably modified type is the point where the sizes in
                    // it are evaluated, whether it declares an object, a pointer to one or a
                    // name for the type.
                    self.variable_length(decl);
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
            Stmt::IndirectGoto(target) => self.indirect_goto(target, span),
            Stmt::Asm(asm) => self.asm(asm, span),
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
                    // A label inside a loop or an `if` that nothing falls into. The construct
                    // still has to be built, because control arrives in the middle of it and
                    // leaves through the parts around it, so a block nothing branches to is
                    // started and the walk goes on from there as if the statement were
                    // reachable. What that builds ahead of the label is reached by nothing and
                    // is taken out with the other unreachable blocks at the end.
                    self.at = Some(self.new_block());
                    self.stmt(id);
                }
            }
        }
    }

    /// `__builtin_va_arg(list, T)`, which is one argument off a variable argument list.
    ///
    /// It stays an intrinsic rather than becoming the loads and the branch it is on the way to
    /// the machine, because which of those it is is the target's answer and this is not where
    /// the target's answers are kept. The list arrives as a pointer, which is what every
    /// target's `va_list` has decayed to by the time anything reads it.
    fn va_arg(&mut self, list: ExprId, ty: TypeId, span: Span) -> Option<Value> {
        let list = self.value(list);
        let result = repr::value_type(self.types(), self.target(), ty)?;
        let mut build = self.build(span);
        let args = build.func().push_values(&[list]);
        Some(build.value(InstData { args, ..InstData::new(Opcode::VaArg) }, result))
    }

    /// The same thing where what is read is a structure or a union, which answers where the
    /// object is rather than what it is.
    ///
    /// An aggregate is not a value and there is nothing for the result of [`Opcode::VaArg`] to
    /// be, so the object form is a second instruction and not a wider reading of the first. The
    /// size and the alignment travel with it because they are what the algorithm steps the list
    /// on by and what a target that has to put registers somewhere needs to know.
    fn va_object(&mut self, list: ExprId, ty: TypeId, span: Span) -> Value {
        let list = self.value(list);
        let size = repr::size_of(self.types(), self.target(), ty);
        let align = repr::align_of(self.types(), self.target(), ty);
        let info = MemInfo { size, align, order: MemOrder::NotAtomic, tbaa: None };
        let mut build = self.build(span);
        let mem = build.func().add_mem(info);
        let args = build.func().push_values(&[list]);
        let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::VaObject) };
        build.value(data, Type::PTR)
    }

    /// `__builtin_va_start`, `__builtin_va_end` and `__builtin_va_copy`, which are the three of
    /// the family that read nothing and answer nothing.
    ///
    /// Each of them is one instruction over the address of a list, for the reason `va_arg` is
    /// one: what the target does to a list is the target's answer. `va_end` is nothing at all on
    /// every psABI in this compiler, and it is still emitted, because it is what says the list
    /// stops being read here and something later may want to know that.
    fn va_effect(&mut self, opcode: Opcode, lists: &[ExprId], span: Span) {
        let lists: Vec<Value> = lists.iter().map(|&list| self.value(list)).collect();
        let mut build = self.build(span);
        let args = build.func().push_values(&lists);
        build.inst(InstData { args, ..InstData::new(opcode) }, &[]);
    }

    /// The statements of a `({ ... })`, with the one that produced its value left undone.
    ///
    /// The scope is opened here and closed by the caller, since the value has to be taken out
    /// before the objects the block declared are given back: `({ int a[n]; a[0]; })` reads the
    /// array while it is still there. What is answered is the last statement when it is an
    /// expression statement, which is where the value of one of these comes from, and nothing
    /// when it is anything else, which is what makes `({ })` and `({ int x; })` both `void`.
    ///
    /// The cursor is left somewhere whatever the statements did, so that the expression this
    /// sits in has a block to be built in. `({ return 1; 0; })` leaves it in a block nothing
    /// branches to, which is what the rest of that expression is, and which is taken out with
    /// the other unreachable blocks at the end.
    fn statements(&mut self, id: StmtId) -> Option<ExprId> {
        let tast = self.tast();
        self.open();
        let mut value = None;
        match tast[id] {
            Stmt::Block(list) => {
                let count = tast[list].len();
                for index in 0..count {
                    let stmt = self.tast()[list][index];
                    match self.tast()[stmt] {
                        Stmt::Expr(expr) if index + 1 == count => value = Some(expr),
                        _ => self.stmt(stmt),
                    }
                }
            }
            // Not a block, which the parser does not build and the checking gives `void`.
            _ => self.stmt(id),
        }
        if self.at.is_none() {
            let dead = self.new_block();
            self.ssa.seal(self.func, dead);
            self.at = Some(dead);
        }
        value
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
        if self.grows {
            // The scopes control lands in, which is what a jump to this label has to put the
            // stack back to. Taken before the labelled statement is walked, since a scope that
            // statement opens is one the label is outside of.
            self.landings.insert(body, self.marks.clone());
        }
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
    ///
    /// What the stack should be on arrival is decided by where the label is, which is not known
    /// here when the label is further down, so the branch is remembered and the restore in front
    /// of it is built at the end.
    fn goto(&mut self, label: rucc_sema::LabelId, span: Span) {
        let Some(body) = self.tast()[label].stmt else {
            // A label used and never defined, which the checking reported. There is nowhere to
            // jump to, and what follows is as unreachable as it would have been.
            self.at = None;
            return;
        };
        let block = self.label_block(body);
        let inst = self.jump(block, span);
        self.pending(inst, vec![body], "a goto in a function with a variable length array", span);
        self.at = None;
    }

    /// Remembers a jump whose stack is settled once the walk knows where its labels are.
    ///
    /// Nothing is remembered for a function whose stack does not move, which is nearly all of
    /// them: there is no restore to build anywhere in one, so there is nothing to come back for.
    fn pending(&mut self, inst: Inst, targets: Vec<StmtId>, what: &'static str, span: Span) {
        if self.grows {
            self.jumps.push(Jump { inst, from: self.marks.clone(), targets, what, span });
        }
    }

    /// Puts the stack back where each jump's label expects it, now that every label is known.
    ///
    /// The restore goes in front of the branch, which is where it would have been built if the
    /// answer had been available there. A computed `goto` gets one restore for all of its
    /// labels, so its labels have to want the same one, and one that lands inside a scope it is
    /// not in is turned down: which label it arrives at is not known until the program runs.
    fn settle(&mut self) {
        let jumps = std::mem::take(&mut self.jumps);
        for jump in jumps {
            let mut wanted = None;
            for target in &jump.targets {
                let arriving = self.landings.get(target).map_or([].as_slice(), Vec::as_slice);
                let wants = landing(&jump.from, arriving);
                wanted = match wanted {
                    // Two labels that want different stacks. One restore cannot be right for
                    // both of them and which one control arrives at is not known here, so this
                    // is turned down for the same reason a jump into a scope is.
                    Some(first) if first != wants => Some(Landing::Enters),
                    _ => Some(wants),
                };
            }
            match wanted {
                Some(Landing::Restore(saved)) => {
                    self.restore_before(saved, jump.inst, jump.span);
                }
                Some(Landing::Enters) => self.unsupported(jump.what, jump.span),
                Some(Landing::Same) | None => {}
            }
        }
    }

    /// `&&name`, GNU's address of a label, which is a value a computed `goto` can jump to.
    ///
    /// The block is the one the label starts, made here when the label has not been reached
    /// yet, and the address of it is not an edge into it: nothing arrives where the address is
    /// taken. The edges are at the `goto *` that uses it.
    fn label_addr(&mut self, label: rucc_sema::LabelId, span: Span) -> Option<Value> {
        let Some(body) = self.tast()[label].stmt else {
            // A label whose address is taken and which is never defined, which the checking
            // reported. There is no block, so there is no address either.
            return Some(self.poison(Type::PTR, span));
        };
        let block = self.label_block(body);
        Some(self.build(span).block_addr(block))
    }

    /// `goto *expr;`, GNU's computed goto, which is a branch to every label the function takes
    /// the address of.
    ///
    /// Which of them it arrives at is the address's business and not the walk's, so all of them
    /// are listed. That is the conservative answer and the only one available: the address can
    /// have been through a table, a parameter or a global on the way here.
    fn indirect_goto(&mut self, target: ExprId, span: Span) {
        let address = self.value(target);
        let taken = self.taken.clone();
        let blocks: Vec<Block> = taken.iter().map(|&body| self.label_block(body)).collect();
        if blocks.is_empty() {
            // Nothing in the function took the address of a label, so the address came from
            // somewhere else, and a jump to a label in another function is undefined. The
            // expression is still evaluated, since it can have side effects in it.
            self.build(span).unreachable();
            self.at = None;
            return;
        }
        let inst = self.build(span).indirect_br(address, &blocks);
        self.ssa.branch(self.func, inst);
        let what = "a computed goto in a function with a variable length array";
        self.pending(inst, taken, what, span);
        self.at = None;
    }

    /// `asm(...)`, GNU's inline assembly.
    ///
    /// The instruction carries one comma separated constraint list in the order the template
    /// numbers its operands, which is the outputs and then the inputs, so `%2` is the third
    /// entry of that list whatever each entry turned out to be. Reading it back is a scan: an
    /// entry that is an output travelling in a register takes the next result, and every other
    /// entry takes the next operand, which is a value for an input and an address for anything
    /// in memory. An output written `+` is read as well as written and so takes both.
    ///
    /// An `asm goto` is a terminator, and its first target is where control arrives when the
    /// assembly does not jump. That is what makes the outputs work: they are written into their
    /// objects in that block, so a label the assembly jumps to is somewhere they never happened,
    /// which is what gcc promises and what the register allocator will have to be told later.
    fn asm(&mut self, id: rucc_sema::AsmId, span: Span) {
        let tast = self.tast();
        let node = tast[id];
        let goto = !tast[node.labels].is_empty();
        if goto && self.grows {
            // The same reason a `goto` is turned down: where the stack should be on arrival
            // depends on the scope the label is in, which the walk does not collect.
            self.unsupported("an asm goto in a function with a variable length array", span);
            return;
        }

        // The constraints of every operand, in the order the template counts them, which is
        // also the order the operands below are built in.
        let mut written = Vec::new();
        for list in [node.outputs, node.inputs] {
            for index in 0..tast[list].len() {
                written.push(self.asm_text(tast[list][index].constraint));
            }
        }
        let constraints = written.join(",");
        let mut clobbers = Vec::with_capacity(tast[node.clobbers].len());
        for index in 0..tast[node.clobbers].len() {
            clobbers.push(self.asm_text(tast[node.clobbers][index]));
        }
        let clobbers = clobbers.join(",");
        let template = self.asm_text(node.template);
        let template = self.unit.names.intern(&template);
        let constraints = self.unit.names.intern(&constraints);
        let clobbers = self.unit.names.intern(&clobbers);

        let mut args = Vec::new();
        let mut results = Vec::new();
        let mut writes = Vec::new();
        for index in 0..tast[node.outputs].len() {
            let operand = tast[node.outputs][index];
            let at = tast.expr_span(operand.value);
            let place = self.place(operand.value);
            if operand.memory {
                let addr = self.address_of(place, at);
                args.push(addr);
                continue;
            }
            let ty = self.value_type(place.ty, at);
            if written[index].starts_with('+') {
                let value = match self.read(place, at) {
                    Some(value) => value,
                    None => self.poison(ty, at),
                };
                args.push(value);
            }
            results.push(ty);
            writes.push(place);
        }
        for index in 0..tast[node.inputs].len() {
            let operand = tast[node.inputs][index];
            let at = tast.expr_span(operand.value);
            if operand.memory {
                let place = self.place(operand.value);
                let addr = self.address_of(place, at);
                args.push(addr);
            } else {
                let value = self.value(operand.value);
                args.push(value);
            }
        }

        // The fall through first and the labels after it, in the order they were written, which
        // is the order `%l0` counts in. A label that was used and never defined was reported by
        // the checking and has no block, so it is not somewhere control can arrive.
        let mut blocks = Vec::new();
        if goto {
            blocks.push(self.new_block());
            for index in 0..tast[node.labels].len() {
                let label = tast[node.labels][index];
                if let Some(body) = tast[label].stmt {
                    blocks.push(self.label_block(body));
                }
            }
        }
        let calls: Vec<BlockCall> =
            blocks.iter().map(|&block| BlockCall { block, args: ValueList::EMPTY }).collect();
        let targets = self.func.push_block_calls(&calls);
        let info = AsmInfo { template, constraints, clobbers, targets };
        let flags = if node.quals.has(AsmQuals::VOLATILE) { Flags::VOLATILE } else { Flags::NONE };
        let inst = self.build(span).inline_asm(info, &args, &results, flags);

        if goto {
            self.ssa.branch(self.func, inst);
            let after = blocks[0];
            self.ssa.seal(self.func, after);
            self.at = Some(after);
        }
        let produced: Vec<Value> = self.func[inst].results().collect();
        for (place, value) in writes.into_iter().zip(produced) {
            self.write(place, value, span);
        }
    }

    /// The text of one of the strings of an assembly statement.
    ///
    /// The elements of a narrow literal are its bytes, and a literal that is not narrow was
    /// reported by the checking, so what comes out of one of those is whatever its elements
    /// spell rather than a second complaint about it.
    fn asm_text(&self, id: rucc_sema::StrId) -> String {
        self.tast()[id].elements.iter().filter_map(|&element| char::from_u32(element)).collect()
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
            self.loops.push(Frame {
                kind: FrameKind::Switch,
                brk: None,
                cont: None,
                depth: self.marks.len(),
            });
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

        self.loops.push(Frame {
            kind: FrameKind::Switch,
            brk: after,
            cont: None,
            depth: self.marks.len(),
        });
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
        self.loops.push(Frame {
            kind: FrameKind::Loop,
            brk: Some(after),
            cont: Some(header),
            depth: self.marks.len(),
        });
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

        self.loops.push(Frame {
            kind: FrameKind::Loop,
            brk: None,
            cont: None,
            depth: self.marks.len(),
        });
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
        // The declarations in the head of a `for` are in a scope of their own, which is what
        // `for (int a[n]; ;)` needs: the object is one object however many times round it goes.
        self.open();
        if let Some(init) = init {
            self.stmt(init);
        }
        if self.at.is_none() {
            self.close(span);
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
        self.loops.push(Frame { kind: FrameKind::Loop, brk: after, cont, depth: self.marks.len() });
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
                self.discard(step);
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
        // The scope the head opened is closed here, where the loop is left, and closing it is
        // not optional even where it saved nothing. The marks are a stack, so a scope opened and
        // not closed is not one leaked mark, it is every close after it taking the wrong mark
        // off: a body that grew the stack gave nothing back, and the restore that should have
        // been at the end of the body ended up after the loop, restoring a pointer saved in a
        // block that does not reach there. That is what the verifier was refusing on
        // `79_vla_continue.c`.
        self.close(span);
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
        // Whatever the scopes being left grew the stack by is given back on the way out, since
        // control is leaving the block the objects were declared in.
        let depth = self.loops[frame].depth;
        self.unwind(depth, span);
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

    /// `return;` or `return expr;`, in whatever form the return value goes back in.
    fn return_stmt(&mut self, value: Option<ExprId>, span: Span) {
        let travel = self.ret.clone();
        let Some(expr) = value else {
            self.build(span).ret(&[]);
            self.at = None;
            return;
        };
        let values = match travel.pass {
            // `return f();` where `f` returns nothing, which is a `void` expression and not a
            // value: it is evaluated and then there is nothing to hand back.
            Pass::Ignore => {
                self.discard(expr);
                Vec::new()
            }
            Pass::Direct => self.eval(expr).into_iter().collect(),
            Pass::Pieces(_) => {
                let place = self.place(expr);
                let addr = self.address_of(place, span);
                self.load_slots(addr, &travel, span)
            }
            // The caller passed somewhere to put it, so returning is writing it there.
            Pass::Reference | Pass::Memory => {
                let place = self.place(expr);
                let from = self.address_of(place, span);
                if let Some(into) = self.sret {
                    self.memcpy(into, from, travel.size, travel.align, span);
                }
                Vec::new()
            }
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
            let ty = self.func.signature().returns[0].ty;
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
            covered += self.stored_size(entry);
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
            self.store_entry(place, entry, span);
        }
    }

    /// How many bytes one initializer entry writes.
    fn stored_size(&mut self, entry: &InitEntry) -> u64 {
        if entry.is_bit_field() {
            // A bit-field writes part of a byte and leaves the rest of it alone, so the byte
            // has to have been zeroed first and this entry covers none of the object.
            return 0;
        }
        let tast = self.tast();
        let ty = tast[entry.value].ty;
        let size = repr::size_of(self.types(), self.target(), ty);
        match tast[entry.value].kind {
            // A string literal shorter than the array it initializes writes what it has, and
            // the rest of the array is zero.
            ExprKind::Str(id) => size.min(tast[id].bytes(self.unit.target).len() as u64),
            _ => size,
        }
    }

    /// One entry of an initializer, at its offset into the object.
    fn store_entry(&mut self, place: Place, entry: InitEntry, span: Span) {
        let tast = self.tast();
        let value = entry.value;
        let ty = tast[value].ty;
        let base = self.address_of(place, span);
        let addr = self.offset(base, entry.offset, span);
        if entry.is_bit_field() {
            // Everything this leaves of the bytes it writes was zeroed above, since a
            // bit-field entry counts as covering none of the object.
            let align = repr::align_of(self.types(), self.target(), place.ty);
            let run = Run::at(align, entry.offset, entry.bit_offset, entry.bit_width);
            if let Some(value) = self.eval(value) {
                self.store_bits(addr, run, ty, value, span);
            }
            return;
        }
        if let ExprKind::Str(id) = tast[value].kind {
            let bytes = tast[id].bytes(self.unit.target).len() as u64;
            let size = bytes.min(repr::size_of(self.types(), self.target(), ty));
            let symbol = self.unit.string(id);
            let source = self.global_addr(symbol, span);
            // The array's own alignment, which both sides of this copy have. An array of
            // characters has one and a wide one has the width of its element, and the object the
            // literal lives in is aligned to the same thing for the same reason, so saying one
            // here is a lie about a fact this already knows and costs a wide copy four moves per
            // character.
            let align = repr::align_of(self.types(), self.target(), ty);
            self.memcpy(addr, source, size, align, span);
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
                None if tast[decl].duration == StorageDuration::Automatic => {
                    // A variable length array whose declaration the walk has not reached, which
                    // a `goto` over it can arrange. The object does not exist yet, so there is
                    // no address to answer with.
                    self.unsupported("a variable length array used before its declaration", span);
                    let addr = self.poison(Type::PTR, span);
                    Place { at: Where::Addr(addr), ty }
                }
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
            // `f().x` and `p = f()`, where what the call produced has to be somewhere before
            // anything can be read out of it. The call writes into a temporary and that is the
            // object, which is what C means by the value of a call having automatic storage
            // duration until the end of the full expression.
            ExprKind::Call { callee, args } => {
                let size = repr::size_of(self.types(), self.target(), ty);
                let align = repr::align_of(self.types(), self.target(), ty);
                let at = self.scratch(size, align, span);
                self.call_into(callee, args, Some(at), span);
                Place { at: Where::Addr(at), ty }
            }
            ExprKind::CompoundLiteral(decl) => self.literal(decl, ty),
            // `(struct S)s`, gcc's cast of a record to its own type, which does nothing at all.
            // Sema lets one through only where the two types are compatible, so the object is
            // the one that was cast rather than a copy of it, the same as the read above. tcc's
            // struct initializer test writes one and so does c-testsuite's copy of it.
            ExprKind::Cast(operand)
                if matches!(self.types().kind(self.types().canonical(ty)), TypeKind::Record(_)) =>
            {
                self.place(operand)
            }
            // One of these whose value is an object rather than a number, `({ s; })` where `s`
            // is a structure. The object is the one the last statement named and not a copy of
            // it, which is what makes `({ s; }).x` read `s`.
            ExprKind::StmtExpr(body) => {
                let last = self.statements(body);
                let at = match last {
                    Some(last) => self.place(last).at,
                    None => {
                        self.unsupported("a statement expression with no value as an object", span);
                        Where::Addr(self.poison(Type::PTR, span))
                    }
                };
                self.close(span);
                Place { at, ty }
            }
            ExprKind::Cond { cond, then, otherwise } => {
                self.conditional_place(cond, then, otherwise, ty, span)
            }
            // One that reads a structure or a union, which answers where the object is. That is
            // an address and so it is a place already, and nothing is copied out of it here:
            // whatever wanted the object reads it where it is, which is the assignment that
            // copies it into a variable or the call that loads it into the registers it travels
            // in.
            ExprKind::VaArg { list } => {
                Place { at: Where::Addr(self.va_object(list, ty, span)), ty }
            }
            // `d = e = a[0] = c` where each of the four is a structure. What an assignment is
            // worth is the value it stored, and the value of an object is the object, so the
            // answer is the place it wrote to rather than a copy of it. That makes a chain of
            // them a sequence of copies out of the one source and needs no temporary anywhere.
            //
            // Only a plain assignment reaches here. A compound one is arithmetic and there is
            // none on an aggregate for it to be, so its value has a type a value can have and
            // nothing asks where that is.
            ExprKind::Assign { op: None, lhs, rhs, .. } => {
                let ty = self.tast()[lhs].ty;
                let into = self.place(lhs);
                self.copy(into, rhs, ty, span);
                into
            }
            _ => {
                // Which is now asked in three places rather than one: an assignment writes
                // through it, and an aggregate passed or returned by value is read through it.
                self.unsupported("this as an object to read or write", span);
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
        let byte = member.offset;
        if let Some(width) = member.bits {
            // The address is of the byte the first of its bits is in, and the run says which
            // bit of that byte it starts at. A member of a record aligned to eight bytes at
            // byte offset four is aligned to four, which is what the run needs to know to say
            // how the loads under it are aligned.
            let base = repr::align_of(self.types(), self.target(), record);
            let addr = self.offset(addr, byte, span);
            let run = Run::at(base, byte, member.bit, width);
            return Place { at: Where::Bits(addr, run), ty };
        }
        let addr = self.offset(addr, byte, span);
        Place { at: Where::Addr(addr), ty }
    }

    /// `base[index]`, where the base is already a pointer to the element type.
    fn element(&mut self, base: ExprId, index: ExprId, ty: TypeId, span: Span) -> Value {
        let pointer = self.value(base);
        let steps = self.value(index);
        let size = self.stride(ty, span);
        let signed = repr::is_signed(self.types(), self.target(), self.tast()[index].ty);
        self.step(pointer, steps, signed, size, false, span)
    }

    /// The address of a place, which every object except a variable in a register has.
    fn address_of(&mut self, place: Place, span: Span) -> Value {
        match place.at {
            Where::Addr(addr) => addr,
            // Nothing should ask: a variable whose address is taken was put in a slot before
            // the walk started, and a bit-field has no address for the program to take.
            Where::Var(_) | Where::Bits(..) => {
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
        size: Stride,
        back: bool,
        span: Span,
    ) -> Value {
        let address = self.address;
        let mut amount = self.widen(steps, signed, address, span);
        match size {
            Stride::Bytes(1) => {}
            Stride::Bytes(bytes) => {
                let mut build = self.build(span);
                let scale = build.iconst(address, i128::from(bytes));
                amount = build.binary(Opcode::Mul, amount, scale, Flags::NSW);
            }
            Stride::Value(scale) => {
                amount = self.build(span).binary(Opcode::Mul, amount, scale, Flags::NSW);
            }
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
            Where::Bits(addr, run) => Some(self.read_bits(addr, run, place.ty, ty, span)),
        }
    }

    /// Writes a place, answering with the bits that went into a bit-field.
    ///
    /// A bit-field is the one place where what was written is not what a read gives back, and
    /// [`Self::write_back`] is what turns those bits into the value that does.
    fn write(&mut self, place: Place, value: Value, span: Span) -> Option<Value> {
        if let TypeKind::Atomic(_) = self.types().kind(self.types().canonical(place.ty)) {
            self.unsupported("an access to an atomic object", span);
        }
        match place.at {
            Where::Var(var) => {
                let block = self.block();
                self.ssa.write(var, block, value);
                None
            }
            Where::Addr(addr) => {
                let info = self.access(place.ty);
                let flags = self.flags(place.ty);
                self.build(span).store(value, addr, info, flags);
                None
            }
            Where::Bits(addr, run) => self.store_bits(addr, run, place.ty, value, span),
        }
    }

    /// Writes a place and answers with what a read of it gives back afterwards.
    ///
    /// That is the value written everywhere except in a bit-field, where it is what fits:
    /// `x.b = 9` on a three bit field is 1, and that is the value of the assignment as well as
    /// the value in the field. Building it takes a shift, so a caller with no use for it says
    /// so and gets back what it wrote.
    fn write_back(&mut self, place: Place, value: Value, want: bool, span: Span) -> Value {
        let kept = self.write(place, value, span);
        if !want {
            return value;
        }
        let (Some(kept), Where::Bits(_, run)) = (kept, place.at) else { return value };
        let signed = repr::is_signed(self.types(), self.target(), place.ty);
        let back = if signed { self.narrow(kept, 0, run.width, true, span) } else { kept };
        self.widen(back, signed, self.func[value].ty, span)
    }

    // Bit-fields.

    /// Reads a run of bits as a value of the type the member was declared with.
    fn read_bits(&mut self, addr: Value, run: Run, ty: TypeId, into: Type, span: Span) -> Value {
        if !self.usable(run, span) {
            return self.poison(into, span);
        }
        let unit = Type::int(run.unit());
        let flags = self.flags(ty);
        let mut whole = None;
        for piece in run.pieces() {
            let part = self.load_piece(addr, piece, flags, span);
            let part = self.widen(part, false, unit, span);
            let part = self.shift(Opcode::Shl, part, piece.offset as u32 * 8, span);
            whole = Some(match whole {
                None => part,
                Some(sofar) => self.build(span).binary(Opcode::Or, sofar, part, Flags::NONE),
            });
        }
        let whole = whole.expect("a run of at least one bit lies in at least one byte");
        let signed = repr::is_signed(self.types(), self.target(), ty);
        let value = self.narrow(whole, run.start, run.width, signed, span);
        self.widen(value, signed, into, span)
    }

    /// Writes a run of bits, leaving every byte it has no bit in as it was.
    ///
    /// Answers with what went in, in the width the pieces were assembled in, which is what a
    /// read of the field afterwards gives back once its top bit has been copied up. Nothing
    /// when the run was reported.
    fn store_bits(
        &mut self,
        addr: Value,
        run: Run,
        ty: TypeId,
        value: Value,
        span: Span,
    ) -> Option<Value> {
        if !self.usable(run, span) {
            return None;
        }
        let unit = Type::int(run.unit());
        let flags = self.flags(ty);
        let signed = repr::is_signed(self.types(), self.target(), ty);
        // What the field keeps of the value, cleared above its width rather than sign
        // extended, because those bits belong to whatever else lives in these bytes.
        let wide = self.widen(value, signed, unit, span);
        let kept = self.narrow(wide, 0, run.width, false, span);
        let placed = self.shift(Opcode::Shl, kept, run.start, span);
        for piece in run.pieces() {
            let part = self.shift(Opcode::LShr, placed, piece.offset as u32 * 8, span);
            let part = self.widen(part, false, Type::int(piece.size * 8), span);
            let stored = if piece.whole() {
                // Nothing but the field is in this piece, so what was there does not matter.
                part
            } else {
                let old = self.load_piece(addr, piece, flags, span);
                let ty = self.func[old].ty;
                let keep = self.build(span).iconst(ty, !piece.mask() as i128);
                let old = self.build(span).binary(Opcode::And, old, keep, Flags::NONE);
                self.build(span).binary(Opcode::Or, old, part, Flags::NONE)
            };
            self.store_piece(addr, piece, stored, flags, span);
        }
        Some(kept)
    }

    /// Whether an access to a run can be built, reporting it when it cannot.
    fn usable(&mut self, run: Run, span: Span) -> bool {
        if !self.target().little_endian {
            // The layout numbers a member's bits from the low bit of its lowest byte, which is
            // not where a big-endian target starts counting.
            self.unsupported("a bit-field on a big-endian target", span);
            return false;
        }
        if !run.accessible() {
            self.unsupported("a bit-field that lies in more than eight bytes", span);
            return false;
        }
        true
    }

    /// One of the loads the bytes under a run are read by.
    fn load_piece(&mut self, addr: Value, piece: Piece, flags: Flags, span: Span) -> Value {
        let addr = self.offset(addr, piece.offset, span);
        let info = MemInfo { size: 0, align: piece.align, order: MemOrder::NotAtomic, tbaa: None };
        self.build(span).load(Type::int(piece.size * 8), addr, info, flags)
    }

    /// One of the stores the bytes under a run are written by.
    fn store_piece(&mut self, addr: Value, piece: Piece, value: Value, flags: Flags, span: Span) {
        let addr = self.offset(addr, piece.offset, span);
        let info = MemInfo { size: 0, align: piece.align, order: MemOrder::NotAtomic, tbaa: None };
        self.build(span).store(value, addr, info, flags);
    }

    /// The bits of a run taken out of the integer they were loaded in.
    ///
    /// The first of them ends up at the bottom and everything above the last of them is gone,
    /// by copying the top one up when the field is signed and by clearing when it is not.
    fn narrow(&mut self, value: Value, start: u32, width: u32, signed: bool, span: Span) -> Value {
        let bits = self.func[value].ty.bits();
        if signed {
            // Left until the field's top bit is the top bit and then arithmetic right, which
            // is how a value is sign extended from a width no type has.
            let value = self.shift(Opcode::Shl, value, bits - start - width, span);
            return self.shift(Opcode::AShr, value, bits - width, span);
        }
        let value = self.shift(Opcode::LShr, value, start, span);
        if start + width == bits {
            return value;
        }
        let ones = if width >= 128 { u128::MAX } else { (1u128 << width) - 1 };
        let ty = self.func[value].ty;
        let mask = self.build(span).iconst(ty, ones as i128);
        self.build(span).binary(Opcode::And, value, mask, Flags::NONE)
    }

    /// A shift by a constant number of bits, which is the value itself when that is none.
    fn shift(&mut self, opcode: Opcode, value: Value, amount: u32, span: Span) -> Value {
        if amount == 0 {
            return value;
        }
        let ty = self.func[value].ty;
        let mut build = self.build(span);
        let amount = build.iconst(ty, i128::from(amount));
        build.binary(opcode, value, amount, Flags::NONE)
    }

    // Expressions.

    /// An expression evaluated for what it does rather than for what it is worth.
    ///
    /// Only an assignment cares about the difference, and only where it writes a bit-field:
    /// what one of those is worth is the value in the field afterwards, which is not the value
    /// that went in and which a statement has no use for.
    fn discard(&mut self, expr: ExprId) {
        let tast = self.tast();
        if let ExprKind::Assign { op, computation, lhs, rhs } = tast[expr].kind {
            let span = tast.expr_span(expr);
            self.assign(op, computation, lhs, rhs, false, span);
            return;
        }
        self.eval(expr);
    }

    /// The value of an expression, which is [`None`] only when it has none.
    fn eval(&mut self, expr: ExprId) -> Option<Value> {
        // The size of a variable length array, which was evaluated where the declaration was
        // reached. `sizeof a` is built by the checking out of the very expression the type
        // points at, so meeting it again here is meeting the same node, and what it is worth is
        // what it was worth then rather than what `n` says now.
        if let Some(&value) = self.vlas.get(&expr) {
            return Some(value);
        }
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
                self.assign(op, computation, lhs, rhs, true, span)
            }
            ExprKind::Cond { cond, then, otherwise } => {
                self.conditional(cond, then, otherwise, ty, span)
            }
            ExprKind::Comma { lhs, rhs } => {
                self.discard(lhs);
                self.eval(rhs)
            }
            ExprKind::Cast(operand) => {
                let from = tast[operand].ty;
                if matches!(self.types().kind(self.types().canonical(ty)), TypeKind::Void) {
                    self.discard(operand);
                    return None;
                }
                let value = self.eval(operand)?;
                Some(self.coerce(value, from, ty, span))
            }
            ExprKind::Convert { kind, operand } => self.convert(kind, operand, ty, span),
            ExprKind::StmtExpr(body) => {
                let last = self.statements(body);
                let value = last.and_then(|last| self.eval(last));
                self.close(span);
                value
            }
            ExprKind::LabelAddr(label) => self.label_addr(label, span),
            ExprKind::VaArg { list } => self.va_arg(list, ty, span),
            ExprKind::VaStart { list } => {
                self.va_effect(Opcode::VaStart, &[list], span);
                None
            }
            ExprKind::VaEnd { list } => {
                self.va_effect(Opcode::VaEnd, &[list], span);
                None
            }
            ExprKind::VaCopy { dst, src } => {
                self.va_effect(Opcode::VaCopy, &[dst, src], span);
                None
            }
            // One bit, and C says the type of the answer is `int`, the same as a comparison.
            ExprKind::Classify { op, lhs, rhs } => {
                let bit = self.classify(op, lhs, rhs, span);
                let into = self.value_type(ty, span);
                Some(self.widen(bit, false, into, span))
            }
            ExprKind::Sign { op, lhs, rhs } => Some(self.sign(op, lhs, rhs, span)),
            // A promise and not a computation, so it is written where it was written and read by
            // whoever comes to read promises. The block goes on: what ends a block is a
            // terminator, and this is not one, so the statement after a `__builtin_unreachable()`
            // is lowered the way it would have been without it. That is the conservative half of
            // the pair, and the half that is right until something acts on the promise.
            ExprKind::Unreachable => {
                self.build(span).inst(InstData::new(Opcode::UnreachableHint), &[]);
                None
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
            ExprKind::Classify { op, lhs, rhs } => self.classify(op, lhs, rhs, span),
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
                self.discard(operand);
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
            let size = self.stride(pointee, span);
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
        // What a prefix one is worth is the value in the object afterwards, which in a
        // bit-field is what fits in it: `++b` on a five bit field holding 31 is 0. A postfix
        // one is worth what was there before and has no use for it.
        let new = self.write_back(place, new, !op.is_postfix(), span);
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
        let size = self.stride(self.pointee(ty), span);
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
        let size = self.stride(pointee, span);
        let left = self.value(lhs);
        let right = self.value(rhs);
        let address = self.address;
        let mut build = self.build(span);
        let left = build.unary(Opcode::PtrToInt, left, address);
        let right = build.unary(Opcode::PtrToInt, right, address);
        let bytes = build.binary(Opcode::Sub, left, right, Flags::NONE);
        let elements = match size {
            Stride::Bytes(0 | 1) => bytes,
            Stride::Bytes(size) => {
                let scale = build.iconst(address, i128::from(size));
                build.binary(Opcode::SDiv, bytes, scale, Flags::EXACT)
            }
            Stride::Value(scale) => build.binary(Opcode::SDiv, bytes, scale, Flags::EXACT),
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

    /// One of the floating point classification builtins, whose answer is one bit.
    ///
    /// Every one of them is a comparison, because there is nothing else for it to be. The family
    /// exists so that a program can ask about a value without calling anything, and `math.h`
    /// defines the macros of these names as exactly these builtins, so there is no function of
    /// any of those names to reach. `isunordered` and `islessgreater` are predicates the IR's
    /// comparison already has, and the two that ask about a magnitude are written against the
    /// infinities, which are the one constant that is exact in every format and so need no
    /// arithmetic to build.
    ///
    /// The operand is evaluated once however many times it is compared, which is the whole
    /// reason these are nodes rather than a rewriting in the front end: `isnan(f())` calls `f`
    /// once and `f() != f()` calls it twice.
    ///
    /// `signbit` is the one that is not a question about the value. A negative zero compares
    /// equal to a positive one and its sign bit is set, so the question is about the bits, and
    /// the answer is whether the number they spell is negative.
    fn classify(&mut self, op: Classify, lhs: ExprId, rhs: Option<ExprId>, span: Span) -> Value {
        let ty = self.tast()[lhs].ty;
        let value = self.value(lhs);
        if let Some(rhs) = rhs {
            let right = self.value(rhs);
            let pred = match op {
                Classify::Unordered => FloatPred::Uno,
                // `a < b || a > b`, which the IR has in one predicate. Not `a != b`, which is
                // true when the two are unordered and so is true of a NaN.
                _ => FloatPred::One,
            };
            return self.build(span).fcmp(pred, value, right, Flags::NONE);
        }
        let ir = self.func[value].ty;
        if op == Classify::SignBit {
            let bits = Type::int(ir.lane().bits());
            let mut build = self.build(span);
            let number = build.unary(Opcode::Bitcast, value, bits);
            let zero = build.iconst(bits, 0);
            return build.icmp(IntPred::Slt, number, zero);
        }
        if op == Classify::Nan {
            // Unordered with itself, which no other value is.
            return self.build(span).fcmp(FloatPred::Uno, value, value, Flags::NONE);
        }
        let Some(format) = repr::float_format_of(self.types(), self.target(), ty) else {
            self.unsupported("classifying a value of this type", span);
            return self.poison(Type::I1, span);
        };
        let up = Real::infinity(format, false).to_bits();
        let down = Real::infinity(format, true).to_bits();
        let mut build = self.build(span);
        let up = build.fconst(ir, up);
        let down = build.fconst(ir, down);
        match op {
            Classify::Infinite => {
                let above = build.fcmp(FloatPred::Oeq, value, up, Flags::NONE);
                let below = build.fcmp(FloatPred::Oeq, value, down, Flags::NONE);
                build.binary(Opcode::Or, above, below, Flags::NONE)
            }
            // Strictly between the two infinities. A NaN is neither, because an ordered
            // comparison against one is false, which is what makes this one test and not two.
            _ => {
                let above = build.fcmp(FloatPred::Olt, down, value, Flags::NONE);
                let below = build.fcmp(FloatPred::Olt, value, up, Flags::NONE);
                build.binary(Opcode::And, above, below, Flags::NONE)
            }
        }
    }

    /// `__builtin_fabs` and `__builtin_copysign`, which are the sign bit and nothing else.
    ///
    /// Both are in the math library rather than the C one, so a call left behind here would not
    /// link for a program that never asked for `-lm`, and neither one needs anything the library
    /// has. What each is, is a mask: `fabs` clears the sign bit and `copysign` takes it from the
    /// second operand, and every other bit of the first operand goes through untouched.
    ///
    /// The bits and not the value, because that is what the operations are. `fabs` of a nan is
    /// that nan with its sign cleared, payload and all, and a negative zero has a sign bit to
    /// clear while comparing equal to a positive zero, so nothing written with comparisons and
    /// negation gives the right answer for either.
    ///
    /// The one format this has to be careful about is the x87 one, whose value is eighty bits
    /// sitting in an object of sixteen. The bitcast is to an integer as wide as the value, not as
    /// wide as the object, so the padding is not part of what is masked and does not come back.
    fn sign(&mut self, op: Sign, lhs: ExprId, rhs: Option<ExprId>, span: Span) -> Value {
        let value = self.value(lhs);
        let from = match op {
            Sign::Clear => None,
            Sign::Of => Some(self.value(rhs.expect("copysign takes a second operand"))),
        };
        let float = self.func[value].ty;
        let bits = Type::int(float.lane().bits());
        // The sign bit of the format, which is the highest bit of the value in every one of them.
        let top = 1i128 << (bits.bits() - 1);
        let rest = top.wrapping_sub(1);
        let mut build = self.build(span);
        let number = build.unary(Opcode::Bitcast, value, bits);
        let mask = build.iconst(bits, rest);
        let magnitude = build.binary(Opcode::And, number, mask, Flags::NONE);
        let whole = match from {
            None => magnitude,
            Some(from) => {
                let number = build.unary(Opcode::Bitcast, from, bits);
                let mask = build.iconst(bits, top);
                let sign = build.binary(Opcode::And, number, mask, Flags::NONE);
                build.binary(Opcode::Or, magnitude, sign, Flags::NONE)
            }
        };
        build.unary(Opcode::Bitcast, whole, float)
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
        let (var, join) = self.branch(cond, [then, otherwise], span, |body, arm| {
            let value = body.eval(arm);
            // An arm of a type that has no value is evaluated for its effects and has nothing
            // to carry to the join, which is `c ? f() : g()` where both of them answer `void`.
            into.and(value)
        })?;
        let into = into?;
        Some(self.ssa.read(self.func, var, join, into))
    }

    /// One of these whose value is an object, which is a structure or a union.
    ///
    /// The answer is the address of whichever arm was taken and not a copy of it into a third
    /// place. C makes the value an rvalue that may not be assigned to, and the arms are already
    /// objects that outlive the full expression, so a copy would be a copy nothing could
    /// observe. SQLite's parser writes one of these, which is what asked for it.
    fn conditional_place(
        &mut self,
        cond: ExprId,
        then: ExprId,
        otherwise: ExprId,
        ty: TypeId,
        span: Span,
    ) -> Place {
        let at = self.branch(cond, [then, otherwise], span, |body, arm| {
            let place = body.place(arm);
            // An arm that does not come back has no address to answer with, and the branch
            // below is about to throw the arm away rather than join it.
            body.at?;
            Some(body.address_of(place, span))
        });
        let addr = match at {
            Some((var, join)) => self.ssa.read(self.func, var, join, Type::PTR),
            None => self.poison(Type::PTR, span),
        };
        Place { at: Where::Addr(addr), ty }
    }

    /// The shape both conditionals have: the condition, each arm in a block of its own, and a
    /// join that whichever arms came back branch to.
    ///
    /// What an arm contributes is a value, which is the value of the arm in one case and the
    /// address of the object it names in the other, and it goes into a variable the caller
    /// reads at the join with whatever type it is expecting.
    ///
    /// Answers the variable and the join, and nothing when neither arm reached one, which is
    /// `c ? exit(1) : abort()` where both of them are `_Noreturn`.
    fn branch(
        &mut self,
        cond: ExprId,
        arms: [ExprId; 2],
        span: Span,
        mut of: impl FnMut(&mut Self, ExprId) -> Option<Value>,
    ) -> Option<(Var, Block)> {
        let value = self.condition(cond);
        let then_block = self.new_block();
        let else_block = self.new_block();
        self.br_if(value, then_block, else_block, span);
        self.ssa.seal(self.func, then_block);
        self.ssa.seal(self.func, else_block);

        let var = self.temp();
        let mut join = None;
        for (block, arm) in [then_block, else_block].into_iter().zip(arms) {
            self.at = Some(block);
            let value = of(self, arm);
            if self.at.is_none() {
                continue;
            }
            if let Some(value) = value {
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
        Some((var, join))
    }

    /// An assignment, plain or compound.
    fn assign(
        &mut self,
        op: Option<BinaryOp>,
        computation: TypeId,
        lhs: ExprId,
        rhs: ExprId,
        want: bool,
        span: Span,
    ) -> Option<Value> {
        let ty = self.tast()[lhs].ty;
        let place = self.place(lhs);
        let Some(op) = op else {
            if repr::value_type(self.types(), self.target(), ty).is_none() {
                return self.copy(place, rhs, ty, span);
            }
            let value = self.eval(rhs)?;
            return Some(self.write_back(place, value, want, span));
        };

        // `a op= b` is not `a = a op b` with the conversions left out: the operation happens in
        // the computation type and the answer is converted back, which is why `i /= 0.5` on an
        // `int` divides in `double`.
        let old = self.read(place, span)?;
        let old = self.coerce(old, ty, computation, span);
        let value = if self.is_pointer(computation) {
            let steps = self.value(rhs);
            let index = self.tast()[rhs].ty;
            let size = self.stride(self.pointee(computation), span);
            let signed = repr::is_signed(self.types(), self.target(), index);
            self.step(old, steps, signed, size, op == BinaryOp::Sub, span)
        } else {
            let right = self.value(rhs);
            self.arithmetic(op, old, right, computation, span)
        };
        let value = self.coerce(value, computation, ty, span);
        Some(self.write_back(place, value, want, span))
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

    /// A call, whose value is a value when the return type has one.
    fn call(&mut self, callee: ExprId, args: rucc_sema::ExprList, span: Span) -> Option<Value> {
        self.call_into(callee, args, None, span)
    }

    /// A call, direct when the callee is a function and indirect when it is a pointer.
    ///
    /// `into` is where a return value that is an object goes, which the caller of this knows and
    /// the call does not: it is the temporary behind `f().x`, or the object of `p = f()`. A call
    /// whose value nobody wants still passes somewhere to put it when the ABI says the callee
    /// writes it, because the callee writes it either way.
    fn call_into(
        &mut self,
        callee: ExprId,
        args: rucc_sema::ExprList,
        into: Option<Value>,
        span: Span,
    ) -> Option<Value> {
        if self.missing_builtin(callee, span) {
            return None;
        }
        let tast = self.tast();
        let ty = tast[callee].ty;
        let count = tast[args].len();
        let actual: Vec<TypeId> = (0..count).map(|index| tast[tast[args][index]].ty).collect();
        let plan = self.unit.call_plan(ty, &actual, span)?;

        let mut values = Vec::with_capacity(count + 1);
        let destination = if plan.returns_through_memory() {
            let at = match into {
                Some(at) => at,
                None => self.scratch(plan.ret.size, plan.ret.align, span),
            };
            values.push(at);
            Some(at)
        } else {
            into
        };

        for index in 0..count {
            let arg = tast[args][index];
            let travel = &plan.args[index];
            match travel.pass {
                // Nothing of it travels, and it is still evaluated: `f(g())` calls `g`.
                Pass::Ignore => {
                    self.discard(arg);
                }
                Pass::Direct => {
                    let value = self.value(arg);
                    values.push(value);
                }
                Pass::Pieces(_) => {
                    let place = self.place(arg);
                    let addr = self.address_of(place, span);
                    let slots = self.load_slots(addr, travel, span);
                    values.extend(slots);
                }
                // The callee is given the address of a copy and may write to it, so the copy is
                // made here and the object the program wrote is not what travels.
                Pass::Reference => {
                    let place = self.place(arg);
                    let from = self.address_of(place, span);
                    let copy = self.scratch(travel.size, travel.align, span);
                    self.memcpy(copy, from, travel.size, travel.align, span);
                    values.push(copy);
                }
                // The object's own bytes go in the argument area, which is what `byval` on the
                // parameter says and what the backend does, so what travels is where they are.
                Pass::Memory => {
                    let place = self.place(arg);
                    let addr = self.address_of(place, span);
                    values.push(addr);
                }
            }
        }

        let direct = self.direct(callee, &plan, &actual, span);
        let inst = match direct {
            Some((symbol, settled)) => {
                let sig = self.func.add_signature(settled.signature);
                self.build(span).call_varargs(symbol, sig, &values, &settled.varargs)
            }
            None => {
                let addr = self.value(callee);
                let mut build = self.build(span);
                let sig = build.func().add_signature(plan.signature.clone());
                let varargs = build.func().push_abis(&plan.varargs);
                let info =
                    build.func().add_call(CallInfo { callee: None, signature: sig, varargs });
                let returns: Vec<Type> = build.func()[sig].return_types().collect();
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

        match plan.ret.pass {
            Pass::Ignore => None,
            Pass::Direct => {
                let value = self.func[inst].results().next();
                if let (Some(at), Some(value)) = (destination, value) {
                    let info = self.piece_info(plan.ret.align, 0);
                    self.build(span).store(value, at, info, Flags::NONE);
                }
                value
            }
            // The object came back in registers, which are written into whatever wanted it. A
            // call whose value nobody wants leaves them where they are.
            Pass::Pieces(_) => {
                let at = destination?;
                let results: Vec<Value> = self.func[inst].results().collect();
                self.store_slots(at, &plan.ret, &results, span);
                None
            }
            // The callee wrote it where it was told to, so there is nothing to hand back.
            Pass::Reference | Pass::Memory => None,
        }
    }

    /// The name a call goes to and the signature to call it with, when it goes to a name rather
    /// than through a pointer.
    ///
    /// The two are answered together because they can disagree. `int f();` takes whatever it is
    /// given, so a call written below it is checked against no parameter list at all, and an
    /// `int f(int, int)` further down is what the function ends up with. The call site is not
    /// wrong and neither is the definition, and the signature the call is written with has to be
    /// the function's or the call goes to a function with another one. So the declaration is
    /// asked what it settled on, and where the values travel the same way either the settled
    /// signature is the one used.
    ///
    /// [`None`] when they do not travel the same way, which is `f(1, 2, 3)` against an
    /// `int f(int, int)`. That call is undefined behaviour if control ever arrives at it and
    /// that is the programmer's to answer for; refusing to translate the file is not. It goes
    /// through the function's address with the signature the call site had, which is the shape
    /// a call through a function pointer already needs.
    fn direct(
        &mut self,
        callee: ExprId,
        plan: &Plan,
        actual: &[TypeId],
        span: Span,
    ) -> Option<(rucc_base::Symbol, Plan)> {
        let tast = self.tast();
        let ExprKind::Convert { kind: Conversion::FunctionDecay, operand } = tast[callee].kind
        else {
            return None;
        };
        let ExprKind::Decl(decl) = tast[operand].kind else { return None };
        // The declaration carries the type the whole file settled on and the expression carries
        // the one that was in scope where the call was written, which is how the two differ.
        let declared = self.tast()[decl].ty;
        let settled = self.unit.plan(declared, actual, span)?;
        let alike = settled.args.len() == plan.args.len()
            && travels_alike(&settled.ret, &plan.ret)
            && settled.args.iter().zip(&plan.args).all(|(a, b)| travels_alike(a, b));
        // The travels agreeing is not the whole of it, because an argument past the end of a
        // parameter list travels the same way and still has no parameter to arrive in. So the
        // values are counted against what the signature takes, which is what a call to a name
        // has to hold to and what the verifier reads.
        let passed = usize::from(settled.returns_through_memory())
            + settled.args.iter().map(|travel| travel.types.len()).sum::<usize>();
        let named = settled.signature.params.len();
        let fits = if settled.signature.variadic { passed >= named } else { passed == named };
        if !alike || !fits {
            return None;
        }
        Some((self.unit.symbol_of(decl), settled))
    }

    /// Whether this call goes to a builtin nothing here builds anything for, having reported it.
    ///
    /// A name the walk does not recognise becomes a call to that name, which is right for every
    /// function and wrong for a builtin: no object file defines one, so the program the compiler
    /// wrote down cannot be linked and the name in the linker's complaint is one its author never
    /// typed. Which names those are is [`rucc_sema::unimplemented_builtin`]'s to say, for the
    /// reason [`Unit::library_name`](crate::Unit) asks about a spelling rather than a
    /// declaration: it is a fact about the name and the table it came out of.
    ///
    /// A program that writes its own function with the name gets the function it wrote. That is
    /// not the reason this exists, but a definition in front of us is a definition and the call
    /// to it links.
    fn missing_builtin(&mut self, callee: ExprId, span: Span) -> bool {
        let tast = self.tast();
        let ExprKind::Convert { kind: Conversion::FunctionDecay, operand } = tast[callee].kind
        else {
            return false;
        };
        let ExprKind::Decl(decl) = tast[operand].kind else { return false };
        if tast[decl].body.is_some() {
            return false;
        }
        let Some(name) = tast[decl].name else { return false };
        if !rucc_sema::unimplemented_builtin(self.unit.names.resolve(name)) {
            return false;
        }
        let spelled = self.unit.names.resolve(name).to_string();
        self.unit.missing_builtin(&spelled, span);
        true
    }

    /// Reports a construct the walk does not build IR for yet.
    fn unsupported(&mut self, what: &str, span: Span) {
        self.unit.unsupported(what, span);
    }
}

/// Whether two values travel the same way, which is what says a call written with one signature
/// can be made with the other.
///
/// The pass and the IR types are what the values at the call are built from, and the size and
/// the alignment are what a pass that copies the object reads.
fn travels_alike(a: &Travel, b: &Travel) -> bool {
    a.pass == b.pass && a.types == b.types && a.size == b.size && a.align == b.align
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
    /// The labels the body takes the address of, in the order it takes them.
    taken: Vec<rucc_sema::LabelId>,
}

impl Scan<'_> {
    /// One statement and everything under it.
    fn stmt(&mut self, id: StmtId) {
        match self.tast[id] {
            Stmt::Error | Stmt::Empty | Stmt::Break | Stmt::Continue | Stmt::Goto(_) => {}
            Stmt::Expr(expr) => self.expr(expr),
            Stmt::IndirectGoto(expr) => self.expr(expr),
            Stmt::Asm(asm) => self.asm(asm),
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
            ExprKind::Error
            | ExprKind::Const(_)
            | ExprKind::Str(_)
            | ExprKind::Decl(_)
            | ExprKind::Unreachable => {}
            ExprKind::LabelAddr(label) => {
                if !self.taken.contains(&label) {
                    self.taken.push(label);
                }
            }
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
            ExprKind::VaArg { list } | ExprKind::VaStart { list } | ExprKind::VaEnd { list } => {
                self.escape(list);
                self.expr(list);
            }
            ExprKind::VaCopy { dst, src } => {
                self.escape(dst);
                self.escape(src);
                self.expr(dst);
                self.expr(src);
            }
            ExprKind::Classify { lhs, rhs, .. } | ExprKind::Sign { lhs, rhs, .. } => {
                self.expr(lhs);
                if let Some(rhs) = rhs {
                    self.expr(rhs);
                }
            }
        }
    }

    /// The operands of an assembly statement, which is where an object needs an address
    /// without anything in the program having written `&`.
    ///
    /// Which operands those are was decided by the checking, so the answer here is the same one
    /// the walk will reach, which is the point: an operand the walk takes the address of has to
    /// be one this gave a stack slot to.
    fn asm(&mut self, id: rucc_sema::AsmId) {
        let node = self.tast[id];
        for list in [node.outputs, node.inputs] {
            for index in 0..self.tast[list].len() {
                let operand = self.tast[list][index];
                if operand.memory {
                    self.escape(operand.value);
                }
                self.expr(operand.value);
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
            // An array that decayed is the address of the array, which is how a `va_list` that
            // is an array of one arrives at the operators that write it.
            ExprKind::Convert { kind: Conversion::ArrayDecay, operand } => self.escape(operand),
            _ => {}
        }
    }
}
