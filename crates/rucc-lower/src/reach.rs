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
//! which is [`Decl::retained`] and is the answer for the definitions that are reached from
//! somewhere no C file says.

use std::collections::HashSet;

use rucc_sema::{Decl, DeclId, DeclKind, ExprId, ExprKind, InitList, Linkage, Stmt, StmtId, Tast};

/// The declarations something in the file reaches, given the file.
///
/// Objects are in the answer as well as functions. They are not what the walk is for, since an
/// object with static storage is emitted whether or not anything reads it, but a local `static`
/// with an initializer that names a function is how a reference reaches this from a place that is
/// neither a body nor a file-scope image, so the two kinds travel the same worklist.
#[must_use]
pub(crate) fn reachable(tast: &Tast) -> HashSet<DeclId> {
    let mut walk = Reach { tast, seen: HashSet::new(), work: Vec::new() };
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
    if node.retained {
        return true;
    }
    match node.kind {
        // An object with static storage is emitted whether or not it is read, so whatever its
        // image names is reached. Dropping the ones nothing reads is a separate question with an
        // answer of its own, and until it is asked this has to assume every one of them is there.
        DeclKind::Object => true,
        DeclKind::Function => node.linkage == Linkage::External,
    }
}

/// The walk, and what it has reached so far.
struct Reach<'a> {
    tast: &'a Tast,
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

    /// What one declaration reaches, which is its initializer and its body.
    fn decl(&mut self, decl: DeclId) {
        let node = &self.tast[decl];
        let (init, body) = (node.init, node.body);
        if let Some(init) = init {
            self.init(init);
        }
        if let Some(body) = body {
            self.stmt(body);
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
            Stmt::If { cond, then, otherwise } => {
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
            ExprKind::Error | ExprKind::Const(_) | ExprKind::Str(_) | ExprKind::LabelAddr(_) => {}
            // The one node that is a reference. Whether it is a call, an address or a read is
            // not asked, because a definition has to exist for all three.
            ExprKind::Decl(decl) | ExprKind::CompoundLiteral(decl) => self.mark(decl),
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
            ExprKind::Classify { lhs, rhs, .. } | ExprKind::Sign { lhs, rhs, .. } => {
                self.expr(lhs);
                if let Some(rhs) = rhs {
                    self.expr(rhs);
                }
            }
        }
    }
}
