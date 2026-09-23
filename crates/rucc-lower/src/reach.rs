//! Which declarations the file has a reason to emit.
//!
//! Design: `spec/08-ir.md` section 8.1.
//!
//! A function with internal linkage that nothing in the translation unit refers to cannot be
//! referred to from outside it either, since that is what internal linkage means, so it is a
//! definition of something that can never run. C 6.9p3 requires a definition for a function with
//! internal linkage that is used and asks for nothing at all about one that is not, and no
//! compiler emits them. This is the walk that says which ones are used.
//!
//! It matters more than the object size it saves. A `static inline` in a system header is written
//! once and reaches every file that includes the header, so a program that never asks for a byte
//! swap picks up three functions built out of the byte swap builtins because `<endian.h>` defines
//! them, and a construct this compiler does not lower yet turns a program that never used it into
//! a program that does not build.
//!
//! # What refers to a function
//!
//! Naming it. A call, an address taken, an initializer that mentions it and an operand of an
//! `asm` are all [`ExprKind::Decl`] in the typed tree, and each of them arrives here as the same
//! node, so there is one rule rather than four and nothing is missed by having listed the wrong
//! four. What is not a reference is `sizeof` over a call, which the front end folded before this
//! ran, so the node is gone by the time this looks.
//!
//! The set is transitive rather than one level deep, because two `static` functions may call each
//! other and neither be reachable. So it is a worklist: roots go in, and what a definition names
//! goes in when that definition is reached and not before.
//!
//! # What a root is
//!
//! A function with external linkage, since another translation unit may call it. Every object
//! with static storage, since those are all emitted and a reference from one is a reference. And
//! anything a `used`, `retain`, `constructor`, `destructor` or `alias` attribute asks to be kept,
//! which is [`DeclFlags::RETAINED`] and is the answer for the definitions that are reached from
//! somewhere no C file says.
//!
//! An inline definition is not one of them, for the same reason a `static` function is not. The
//! name it defines is external, but 6.7.4p7 says this unit emits nothing under it, so no object
//! file offers it and nothing outside can be calling this body. What that leaves is a set that
//! means what it says for these as well: a body offered for inlining is in the answer exactly
//! when something in this file names it, which is the question [`crate::unit`] has to ask before
//! it puts a copy of the body out of line.

use std::collections::HashSet;

use rucc_ast::{BinaryOp, UnaryOp};
use rucc_base::Interner;
use rucc_sema::{
    Const, Conversion, Decl, DeclFlags, DeclId, DeclKind, Eval, ExprId, ExprKind, InitList,
    Linkage, Stmt, StmtId, Tast,
};
use rucc_target::TargetInfo;
use rucc_types::Types;

/// The declarations something in the file reaches, given the file.
///
/// Objects are in the answer as well as functions. They are not what the walk is for, since an
/// object with static storage is emitted whether or not anything reads it, but a local `static`
/// with an initializer that names a function is how a reference reaches this from a place that is
/// neither a body nor a file-scope image, so the two kinds travel the same worklist.
#[must_use]
pub(crate) fn reachable(decide: Decide<'_>) -> HashSet<DeclId> {
    let tast = decide.tast;
    let mut walk = Reach { tast, decide, seen: HashSet::new(), work: Vec::new() };
    for index in 0..tast.top_level().len() {
        let decl = tast.top_level()[index];
        if is_root(&tast[decl]) {
            walk.mark(decl);
        }
    }
    while let Some(decl) = walk.work.pop() {
        walk.decl(decl);
    }
    walk.seen
}

/// Whether the file has a reason to emit this declaration without anything having named it.
fn is_root(node: &Decl) -> bool {
    if node.flags.contains(DeclFlags::RETAINED) {
        return true;
    }
    match node.kind {
        // An object with static storage is emitted whether or not it is read, so whatever its
        // image names is reached. Dropping the ones nothing reads is a separate question with an
        // answer of its own, and until it is asked this has to assume every one of them is there.
        DeclKind::Object => true,
        // And not an inline definition, which is external and is still not something another
        // unit can reach, because this one emits nothing under the name for it to reach.
        DeclKind::Function => node.linkage == Linkage::External && node.inline.emits(),
        // A name for a type is a place to evaluate a size at, and nothing is emitted for it.
        DeclKind::Type => false,
    }
}

/// The walk, and what it has reached so far.
struct Reach<'a> {
    tast: &'a Tast,
    decide: Decide<'a>,
    seen: HashSet<DeclId>,
    work: Vec<DeclId>,
}

impl Reach<'_> {
    /// Reaches a declaration, which is work to do the first time and nothing after that.
    fn mark(&mut self, decl: DeclId) {
        if self.seen.insert(decl) {
            self.work.push(decl);
        }
    }

    /// What one declaration reaches, which is its initializer, its body and the handler a
    /// `cleanup` attribute on it names.
    ///
    /// The handler is the one reference in the file that is not an expression, so it is reached
    /// here rather than in the walk over the body. It has to be reached at all, because the
    /// handler is usually a `static inline` in the same header as the type it releases and
    /// nothing else in the file names it, and a call this walk has not seen is a call to a
    /// definition the file did not emit.
    fn decl(&mut self, decl: DeclId) {
        let node = &self.tast[decl];
        let (init, body, cleanup) = (node.init, node.body, node.cleanup);
        if let Some(init) = init {
            self.init(init);
        }
        if let Some(body) = body {
            self.stmt(body);
        }
        if let Some(handler) = cleanup {
            self.mark(handler);
        }
    }

    /// What an initializer reaches, which is what each value it stores reaches.
    fn init(&mut self, init: InitList) {
        for index in 0..self.tast[init].len() {
            let entry = self.tast[init][index];
            self.expr(entry.value);
        }
    }

    /// What one statement reaches.
    fn stmt(&mut self, id: StmtId) {
        match self.tast[id] {
            Stmt::Error
            | Stmt::Empty
            | Stmt::Goto(_)
            | Stmt::Break
            | Stmt::Continue
            | Stmt::Return(None) => {}
            Stmt::Expr(value) | Stmt::IndirectGoto(value) | Stmt::Return(Some(value)) => {
                self.expr(value);
            }
            Stmt::While { cond, body } | Stmt::DoWhile { body, cond } => {
                self.expr(cond);
                self.stmt(body);
            }
            Stmt::Block(body) => {
                for index in 0..self.tast[body].len() {
                    let stmt = self.tast[body][index];
                    self.stmt(stmt);
                }
            }
            // A declaration in a block reaches whatever its initializer names, and a `static`
            // one of those is an object this file emits, so both kinds go in.
            Stmt::Decls(decls) => {
                for index in 0..self.tast[decls].len() {
                    let decl = self.tast[decls][index];
                    self.mark(decl);
                }
            }
            // The arm lowering drops is not a reference, or a `static inline` called only from
            // it is emitted with a call in it to a function nothing defines. Unless a label is in
            // it, since a `goto` from outside reaches that and lowering keeps what follows it.
            Stmt::If { cond, then, otherwise } => {
                if let Some((effects, taken)) = self.decide.condition(cond) {
                    for effect in effects {
                        self.expr(effect);
                    }
                    let (live, dead) =
                        if taken { (Some(then), otherwise) } else { (otherwise, Some(then)) };
                    for arm in
                        [live, dead.filter(|&dead| self.labelled(dead))].into_iter().flatten()
                    {
                        self.stmt(arm);
                    }
                    return;
                }
                self.expr(cond);
                self.stmt(then);
                if let Some(otherwise) = otherwise {
                    self.stmt(otherwise);
                }
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
            // The case table holds the statements the body already holds, so the body alone is
            // walked and nothing is reached twice.
            Stmt::Switch { cond, body, .. } => {
                self.expr(cond);
                self.stmt(body);
            }
            Stmt::Case { body, .. } | Stmt::Default { body } | Stmt::Label { body, .. } => {
                self.stmt(body);
            }
            Stmt::Asm(asm) => {
                let node = self.tast[asm];
                for list in [node.outputs, node.inputs] {
                    for index in 0..self.tast[list].len() {
                        let operand = self.tast[list][index];
                        self.expr(operand.value);
                    }
                }
            }
        }
    }

    /// What one expression reaches.
    fn expr(&mut self, id: ExprId) {
        match self.tast[id].kind {
            ExprKind::Error
            | ExprKind::Const(_)
            | ExprKind::Str(_)
            | ExprKind::LabelAddr(_)
            | ExprKind::Unreachable
            | ExprKind::Trap
            | ExprKind::FrameAddress { .. }
            | ExprKind::ThreadPointer => {}
            // The one node that is a reference. Whether it is a call, an address or a read is
            // not asked, because a definition has to exist for all three.
            ExprKind::Decl(decl) | ExprKind::CompoundLiteral(decl) => self.mark(decl),
            ExprKind::StmtExpr(body) => self.stmt(body),
            // The right side of a chain its left side already ended is not lowered either.
            ExprKind::Binary { op: op @ (BinaryOp::LogAnd | BinaryOp::LogOr), lhs, rhs } => {
                self.expr(lhs);
                let ends = op == BinaryOp::LogOr;
                if self.decide.condition(lhs).is_none_or(|(_, answer)| answer != ends) {
                    self.expr(rhs);
                }
            }
            ExprKind::Alloca { size } => self.expr(size),
            ExprKind::Member { base, .. }
            | ExprKind::Cast(base)
            | ExprKind::VaArg { list: base }
            | ExprKind::VaStart { list: base }
            | ExprKind::VaEnd { list: base }
            | ExprKind::Convert { operand: base, .. }
            | ExprKind::Abs { operand: base }
            | ExprKind::Prefetch { address: base, .. }
            | ExprKind::ObjectSize { address: base, .. }
            | ExprKind::Jump { buffer: base, .. }
            | ExprKind::ByteSwap { operand: base }
            | ExprKind::BitCount { operand: base, .. }
            | ExprKind::Unary { operand: base, .. } => self.expr(base),
            ExprKind::Subscript { base: lhs, index: rhs }
            | ExprKind::Binary { lhs, rhs, .. }
            | ExprKind::Assign { lhs, rhs, .. }
            | ExprKind::VaCopy { dst: lhs, src: rhs }
            | ExprKind::Expect { value: lhs, hint: rhs, .. }
            | ExprKind::Comma { lhs, rhs } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Call { callee, args } => {
                self.expr(callee);
                for index in 0..self.tast[args].len() {
                    let arg = self.tast[args][index];
                    self.expr(arg);
                }
            }
            ExprKind::Cond { cond, then, otherwise } => {
                self.expr(cond);
                self.expr(then);
                self.expr(otherwise);
            }
            ExprKind::Overflow { args, .. } | ExprKind::Atomic { args, .. } => {
                for index in 0..self.tast[args].len() {
                    let arg = self.tast[args][index];
                    self.expr(arg);
                }
            }
            ExprKind::Classify { lhs, rhs, .. } | ExprKind::Sign { lhs, rhs, .. } => {
                self.expr(lhs);
                if let Some(rhs) = rhs {
                    self.expr(rhs);
                }
            }
            ExprKind::FpClassify { value, answers } => {
                self.expr(value);
                for index in 0..self.tast[answers].len() {
                    let answer = self.tast[answers][index];
                    self.expr(answer);
                }
            }
        }
    }
}

impl Reach<'_> {
    /// Whether a statement has a place in it that control can arrive at from outside.
    ///
    /// A `case` counts as well as a label, since the `switch` it belongs to may be outside the
    /// statement. A label anywhere in it is enough, which is more than lowering keeps and never
    /// less.
    fn labelled(&self, id: StmtId) -> bool {
        match self.tast[id] {
            Stmt::Case { .. } | Stmt::Default { .. } | Stmt::Label { .. } => true,
            Stmt::Block(body) => {
                (0..self.tast[body].len()).any(|index| self.labelled(self.tast[body][index]))
            }
            Stmt::If { then, otherwise, .. } => {
                self.labelled(then) || otherwise.is_some_and(|otherwise| self.labelled(otherwise))
            }
            Stmt::While { body, .. }
            | Stmt::DoWhile { body, .. }
            | Stmt::For { body, .. }
            | Stmt::Switch { body, .. } => self.labelled(body),
            _ => false,
        }
    }
}

/// What a condition is known to be before the program runs, which lowering and the walk above
/// both ask so that they agree on which code is never lowered.
#[derive(Clone, Copy)]
pub(crate) struct Decide<'a> {
    tast: &'a Tast,
    types: &'a Types,
    target: &'a TargetInfo,
    names: &'a Interner,
}

impl<'a> Decide<'a> {
    pub(crate) fn new(
        tast: &'a Tast,
        types: &'a Types,
        target: &'a TargetInfo,
        names: &'a Interner,
    ) -> Self {
        Self { tast, types, target, names }
    }

    /// Which way a condition goes when nothing it reads can change the answer, and the parts of
    /// it that still have to run. See `Body::decided_condition`.
    pub(crate) fn condition(&self, cond: ExprId) -> Option<(Vec<ExprId>, bool)> {
        if let Some(answer) = self.folded(cond) {
            return Some((Vec::new(), answer));
        }
        let tast = self.tast;
        match tast[cond].kind {
            ExprKind::Convert { kind: Conversion::Bool, operand } => self.condition(operand),
            ExprKind::Unary { op: UnaryOp::Not, operand } => {
                let (effects, answer) = self.condition(operand)?;
                Some((effects, !answer))
            }
            ExprKind::Binary { op: op @ (BinaryOp::LogAnd | BinaryOp::LogOr), lhs, rhs } => {
                let ends = op == BinaryOp::LogOr;
                if let Some((mut effects, answer)) = self.condition(lhs) {
                    if answer == ends {
                        return Some((effects, ends));
                    }
                    let (rest, answer) = self.condition(rhs)?;
                    effects.extend(rest);
                    return Some((effects, answer));
                }
                let (rest, answer) = self.condition(rhs)?;
                if answer != ends {
                    return None;
                }
                let mut effects = vec![lhs];
                effects.extend(rest);
                Some((effects, ends))
            }
            _ => None,
        }
    }

    /// Which way the condition of an `if` goes when it is a constant, and nothing when it is not.
    ///
    /// A program that asks a question about the compiler rather than about its own data writes the
    /// answer as a constant and puts the call that only the other answer supports inside the arm
    /// that is never taken. `if (sizeof (void *) == 4) use_the_32_bit_helper();` in a build for a
    /// 64 bit target is that, and so is every `if (0)` a configure script leaves behind. Emitting
    /// the branch leaves the call referenced, the linker goes looking for a function nobody
    /// defined, and the program does not link. gcc folds the branch away in the front end, so it
    /// links at every level including `-O0`, and this is where rucc does the same. The optimizer
    /// already removed these at `-O1` and above, which is why the failure was only ever seen in a
    /// build that did not ask for optimization.
    ///
    /// Only a number answers, and a fold that went looking for an address does not, even when
    /// what came back is a number. The folder assumes no object is at zero, which is what turns
    /// `if (&a)` into a true it never was asked to prove, and the assumption is wrong for exactly
    /// the symbol a program writes this about: a weak one is at zero when nothing defined it, and
    /// `if (&pthread_create)` is the idiom. That question belongs to the linker and to run time,
    /// so it keeps its branch. A condition the folder had something to say about does not answer
    /// either, since the ordinary path is the one that reports, and taking the answer here would
    /// drop what it reported on the floor.
    fn folded(&self, cond: ExprId) -> Option<bool> {
        let mut eval = Eval::new(self.tast, self.types, self.target, self.names);
        let folded = eval.constant(cond);
        if eval.addressed() || !eval.finish().is_empty() {
            return None;
        }
        match folded {
            Ok(Const::Int(value)) => Some(value != 0),
            Ok(Const::Float(value)) => Some(!value.is_zero()),
            _ => None,
        }
    }
}
