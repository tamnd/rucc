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

use rucc_ast::{BinaryOp, UnaryOp};
use rucc_base::Interner;
use rucc_base::hash::Set;
use rucc_sema::{
    Const, Conversion, Decl, DeclFlags, DeclId, DeclKind, Definition, Eval, ExprId, ExprKind,
    InitList, Linkage, Stmt, StmtId, StrId, Tast,
};
use rucc_target::TargetInfo;
use rucc_types::{Qualifiers, Types};

/// The declarations something in the file reaches, given the file.
///
/// Objects are in the answer as well as functions. They are not what the walk is for, since an
/// object with static storage is emitted whether or not anything reads it, but a local `static`
/// with an initializer that names a function is how a reference reaches this from a place that is
/// neither a body nor a file-scope image, so the two kinds travel the same worklist.
///
/// The second set is the declarations something reached names, which is the first set without
/// the roots and the targets an attribute string reaches: a declaration of an external function
/// is a root whether or not anything calls it, and what a weakref needs to know is whether
/// anything does. See [`crate::unit`]'s `weak_references`.
#[must_use]
pub(crate) fn reachable(decide: Decide<'_>) -> (Set<DeclId>, Set<DeclId>) {
    walk(decide, false, true)
}

/// The declarations something the file has to keep reaches, which is [`reachable`] with an
/// internal object a root only when an attribute asks for it to be kept.
///
/// This is the walk gcc makes over its call graph when it optimizes. An unused `static` object is
/// not something it has to emit, so neither is a function only that object names, nor a `static`
/// in the body of that function, even one written `used`. The kernel's `CLK_OF_DECLARE` in a
/// module is an unused `static const` table naming the init function, and each `pr_err` in that
/// function has a `used` entry for the printk index. gcc's object has none of them, and rucc's had
/// the entries after the functions were dropped. A function that is reached here and later has
/// every call inlined is still reached, which is why this is asked before the optimizer runs and
/// not after.
#[must_use]
pub(crate) fn live(decide: Decide<'_>) -> Set<DeclId> {
    walk(decide, false, false).0
}

/// The walk [`reachable`], [`live`] and [`referred`] share, which for the third leaves out what
/// follows a statement control never comes out of. See [`Reach::pruned`]. `objects` is whether
/// every object with static storage is a root.
fn walk(decide: Decide<'_>, pruned: bool, objects: bool) -> (Set<DeclId>, Set<DeclId>) {
    let tast = decide.tast;
    let mut walk = Reach {
        tast,
        decide,
        pruned,
        seen: Set::default(),
        named: Set::default(),
        work: Vec::new(),
        valued: Set::default(),
    };
    for index in 0..tast.top_level().len() {
        let decl = tast.top_level()[index];
        if is_root(&tast[decl], objects) {
            walk.mark(decl);
        }
    }
    // What an alias points at is emitted because of the alias, and so is reached from it. The
    // name is a string rather than a reference, so it is looked up among the file's names here.
    // Without this the body of the target was written and the `static inline` functions it calls
    // were not, which is how the kernel's `SYSCALL_DEFINE0(vhangup)` came to call a `capable` the
    // file never defined: `__x64_sys_vhangup` is an alias of the `static` function with the body.
    for decl in aliased(tast, decide.names) {
        walk.mark(decl);
    }
    // The same for a `weakref` whose target the file defines, which is a reference by a string
    // as well. A `static` function with no other caller is otherwise dropped, and the weakref is
    // left pointing at a symbol nothing defines.
    for decl in weakly_referred(tast, decide.names) {
        walk.mark(decl);
    }
    while let Some(decl) = walk.work.pop() {
        walk.decl(decl);
    }
    (walk.seen, walk.named)
}

/// The declarations something the file emits names, which is the second set `reachable` hands
/// back: a call or an address in a body that is reached or in an initializer, and not one in an arm
/// a constant condition never takes, in one after a statement control never comes out of or in a
/// `static inline` nothing reaches.
///
/// gcc writes a declaration in the debug information for each of these that another unit defines,
/// from the same walk over its call graph before it optimizes, so a call the optimizer later
/// folded away still has one and a call the compiler made up on its own does not.
#[must_use]
pub fn referred(tast: &Tast, types: &Types, target: &TargetInfo, names: &Interner) -> Set<DeclId> {
    walk(Decide::new(tast, types, target, names), true, true).1
}

/// The declarations an `alias` attribute in the file names, found by the symbol each is emitted
/// under, which is its assembler name when it was given one.
fn aliased(tast: &Tast, names: &Interner) -> Vec<DeclId> {
    let spelled = |id: StrId| -> String {
        tast[id].elements.iter().filter_map(|&unit| char::from_u32(unit)).collect()
    };
    let wanted: Set<String> =
        tast.top_level().iter().filter_map(|&decl| tast[decl].alias).map(spelled).collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    tast.top_level()
        .iter()
        .copied()
        .filter(|&decl| {
            let node = &tast[decl];
            let symbol = match (node.asm_label, node.name) {
                (Some(label), _) => spelled(label),
                (None, Some(name)) => names.resolve(name).to_owned(),
                (None, None) => return false,
            };
            wanted.contains(&symbol)
        })
        .collect()
}

/// The definitions a `weakref` in the file names, found the way [`aliased`] finds its targets.
///
/// A weakref keeps its target as its assembler name, so it is found by that. Only definitions
/// are in the answer: a declaration of the target has nothing to emit, and one reached from here
/// rather than from a use would read as an ordinary reference to it, which is the question
/// [`crate::unit`] asks the reached declarations to decide whether the reference stays weak.
fn weakly_referred(tast: &Tast, names: &Interner) -> Vec<DeclId> {
    let spelled = |id: StrId| -> String {
        tast[id].elements.iter().filter_map(|&unit| char::from_u32(unit)).collect()
    };
    let wanted: Set<String> = tast
        .top_level()
        .iter()
        .filter(|&&decl| tast[decl].flags.contains(DeclFlags::WEAKREF))
        .filter_map(|&decl| tast[decl].asm_label)
        .map(spelled)
        .collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    tast.top_level()
        .iter()
        .copied()
        .filter(|&decl| {
            let node = &tast[decl];
            if node.flags.contains(DeclFlags::WEAKREF) || node.state == Definition::Declared {
                return false;
            }
            let symbol = match (node.asm_label, node.name) {
                (Some(label), _) => spelled(label),
                (None, Some(name)) => names.resolve(name).to_owned(),
                (None, None) => return false,
            };
            wanted.contains(&symbol)
        })
        .collect()
}

/// Whether the file has a reason to emit this declaration without anything having named it, with
/// `objects` saying whether an internal object is one. See [`live`].
fn is_root(node: &Decl, objects: bool) -> bool {
    if node.flags.contains(DeclFlags::RETAINED) {
        return true;
    }
    match node.kind {
        // An object with static storage is emitted whether or not it is read, so whatever its
        // image names is reached. Dropping the ones nothing reads is a separate question with an
        // answer of its own, and until it is asked this has to assume every one of them is there.
        // [`live`] asks it the way gcc does, where an internal one is only kept when named.
        DeclKind::Object => objects || node.linkage != Linkage::Internal,
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
    /// Whether to leave out the statements after one control never comes out of, a `return`, a
    /// call to a `noreturn` function or an `if (1) return;` among them, up to the next label.
    ///
    /// gcc throws those away when it builds the flow graph, before it looks at what the calls in
    /// a body are, so it writes no declaration for a function only they call. `cleanup_lazy_tlbs`
    /// in `kernel/fork.c` returns early unless a config option is set, and the call after it is
    /// how `smp_cond_func_t` got into rucc's BTF and not gcc's. Only the debug information asks
    /// for this. Lowering keeps those statements, so what it emits is walked in full.
    pruned: bool,
    seen: Set<DeclId>,
    /// What an expression in something reached names, which is what a use is.
    named: Set<DeclId>,
    work: Vec<DeclId>,
    /// The statements a statement expression is worth the value of, which are not thrown away
    /// for doing nothing because what they work out is the answer.
    valued: Set<StmtId>,
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
            self.named.insert(handler);
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
            // gcc throws away a statement that does nothing while it builds the body, before it
            // asks what the body names, so `(void)var` is no reference to `var`. That is how the
            // kernel's `lockdep_assert_in_rcu_read_lock()` reads `RCU`, an `extern` pointer to a
            // structure that exists for the lock checker, and how `__ctx_lock_RCU` got into
            // rucc's BTF and not gcc's.
            Stmt::Expr(value) if self.pruned && !self.valued.contains(&id) && self.inert(value) => {
            }
            Stmt::Expr(value) | Stmt::IndirectGoto(value) | Stmt::Return(Some(value)) => {
                self.expr(value);
            }
            Stmt::While { cond, body } | Stmt::DoWhile { body, cond } => {
                self.expr(cond);
                self.stmt(body);
            }
            // A declaration after the end is still walked, since a `static` one is emitted
            // wherever it is written.
            Stmt::Block(body) => {
                let mut dead = false;
                for index in 0..self.tast[body].len() {
                    let stmt = self.tast[body][index];
                    if dead && !self.labelled(stmt) {
                        if matches!(self.tast[stmt], Stmt::Decls(_)) {
                            self.stmt(stmt);
                        }
                        continue;
                    }
                    self.stmt(stmt);
                    dead = self.pruned && self.ends(stmt);
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
            | ExprKind::ThreadPointer
            | ExprKind::SpEntry
            | ExprKind::TimeStamp { aux: None }
            | ExprKind::Eflags { value: None }
            | ExprKind::CpuModel { .. }
            | ExprKind::ApplyArgs => {}
            ExprKind::Apply { function, args, .. } => {
                self.expr(function);
                self.expr(args);
            }
            // The one node that is a reference. Whether it is a call, an address or a read is
            // not asked, because a definition has to exist for all three.
            ExprKind::Decl(decl) | ExprKind::CompoundLiteral(decl) => {
                self.named.insert(decl);
                self.mark(decl);
            }
            ExprKind::StmtExpr(body) => {
                if let Stmt::Block(list) = self.tast[body] {
                    let mut last = self.tast[list].last().copied();
                    while let Some(stmt) = last {
                        self.valued.insert(stmt);
                        last = match self.tast[stmt] {
                            Stmt::Case { body, .. }
                            | Stmt::Default { body }
                            | Stmt::Label { body, .. } => Some(body),
                            _ => None,
                        };
                    }
                }
                self.stmt(body);
            }
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
            | ExprKind::ConvertVector { operand: base }
            | ExprKind::Prefetch { address: base, .. }
            | ExprKind::ObjectSize { address: base, .. }
            | ExprKind::ConstantP { value: base }
            | ExprKind::Jump { buffer: base, .. }
            | ExprKind::TimeStamp { aux: Some(base) }
            | ExprKind::Eflags { value: Some(base) }
            | ExprKind::ByteSwap { operand: base }
            | ExprKind::BitCount { operand: base, .. }
            | ExprKind::Unary { operand: base, .. } => self.expr(base),
            ExprKind::Subscript { base: lhs, index: rhs }
            | ExprKind::Binary { lhs, rhs, .. }
            | ExprKind::Assign { lhs, rhs, .. }
            | ExprKind::VaCopy { dst: lhs, src: rhs }
            | ExprKind::Expect { value: lhs, hint: rhs, .. }
            | ExprKind::Complex { real: lhs, imag: rhs }
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
            // The same as an `if`, since lowering drops the arm that does not run in the same way.
            // A label inside a statement expression cannot be jumped to from outside it, so there
            // is nothing in the dead arm to keep.
            ExprKind::Cond { cond, then, otherwise } => {
                if let Some((effects, taken)) = self.decide.condition(cond) {
                    for effect in effects {
                        self.expr(effect);
                    }
                    self.expr(if taken { then } else { otherwise });
                    return;
                }
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
            ExprKind::Classify { lhs, rhs, .. }
            | ExprKind::Sign { lhs, rhs, .. }
            | ExprKind::Elementwise { lhs, rhs, .. } => {
                self.expr(lhs);
                if let Some(rhs) = rhs {
                    self.expr(rhs);
                }
            }
            ExprKind::Shuffle { lhs, rhs, mask } => {
                self.expr(lhs);
                if let Some(rhs) = rhs {
                    self.expr(rhs);
                }
                self.expr(mask);
            }
            ExprKind::ShuffleVector { operands } => {
                let (lhs, rhs) = (self.tast[operands][0], self.tast[operands][1]);
                self.expr(lhs);
                self.expr(rhs);
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

    /// Whether working out an expression does nothing but work it out, which for a statement is
    /// what gcc throws away. No call, no store and no `volatile` read, and anything this does not
    /// know is taken to do something, which only costs a declaration gcc would not have written.
    fn inert(&self, id: ExprId) -> bool {
        let types = self.decide.types;
        if types.quals(types.canonical(self.tast[id].ty)).has(Qualifiers::VOLATILE) {
            return false;
        }
        match self.tast[id].kind {
            ExprKind::Const(_) | ExprKind::Str(_) | ExprKind::Decl(_) | ExprKind::LabelAddr(_) => {
                true
            }
            ExprKind::Member { base, .. }
            | ExprKind::Cast(base)
            | ExprKind::Convert { operand: base, .. } => self.inert(base),
            ExprKind::Unary { op, operand } => {
                !matches!(
                    op,
                    UnaryOp::PreInc | UnaryOp::PreDec | UnaryOp::PostInc | UnaryOp::PostDec
                ) && self.inert(operand)
            }
            ExprKind::Subscript { base: lhs, index: rhs }
            | ExprKind::Binary { lhs, rhs, .. }
            | ExprKind::Comma { lhs, rhs } => self.inert(lhs) && self.inert(rhs),
            ExprKind::Cond { cond, then, otherwise } => {
                self.inert(cond) && self.inert(then) && self.inert(otherwise)
            }
            _ => false,
        }
    }

    /// Whether control never comes out of the end of a statement into the one after it.
    ///
    /// No more than gcc can tell before it optimizes, and never a loop. A `do ... while (0)` is
    /// one when its body is and nothing in it breaks out, which is what the kernel's `BUG()` is.
    fn ends(&self, id: StmtId) -> bool {
        match self.tast[id] {
            Stmt::Goto(_)
            | Stmt::IndirectGoto(_)
            | Stmt::Break
            | Stmt::Continue
            | Stmt::Return(_) => true,
            Stmt::Expr(value) => self.stops(value),
            Stmt::Block(body) => {
                let mut dead = false;
                for index in 0..self.tast[body].len() {
                    let stmt = self.tast[body][index];
                    dead = self.ends(stmt) || dead && !self.labelled(stmt);
                }
                dead
            }
            Stmt::If { cond, then, otherwise } => match self.decide.condition(cond) {
                Some((_, true)) => self.ends(then),
                Some((_, false)) => otherwise.is_some_and(|otherwise| self.ends(otherwise)),
                None => self.ends(then) && otherwise.is_some_and(|otherwise| self.ends(otherwise)),
            },
            Stmt::DoWhile { body, .. } => self.ends(body) && !self.leaves(body),
            Stmt::Case { body, .. } | Stmt::Default { body } | Stmt::Label { body, .. } => {
                self.ends(body)
            }
            _ => false,
        }
    }

    /// Whether a `break` or a `continue` is anywhere in a statement, nested loops included, which
    /// is more than can leave it and never less.
    fn leaves(&self, id: StmtId) -> bool {
        match self.tast[id] {
            Stmt::Break | Stmt::Continue => true,
            Stmt::Block(body) => {
                (0..self.tast[body].len()).any(|index| self.leaves(self.tast[body][index]))
            }
            Stmt::If { then, otherwise, .. } => {
                self.leaves(then) || otherwise.is_some_and(|otherwise| self.leaves(otherwise))
            }
            Stmt::While { body, .. }
            | Stmt::DoWhile { body, .. }
            | Stmt::For { body, .. }
            | Stmt::Switch { body, .. }
            | Stmt::Case { body, .. }
            | Stmt::Default { body }
            | Stmt::Label { body, .. } => self.leaves(body),
            _ => false,
        }
    }

    /// Whether an expression never gives control back: `__builtin_unreachable()`, or a call by
    /// name to a function declared `noreturn`.
    fn stops(&self, id: ExprId) -> bool {
        match self.tast[id].kind {
            ExprKind::Unreachable => true,
            ExprKind::Comma { rhs, .. } => self.stops(rhs),
            ExprKind::Call { callee, .. } => {
                let ExprKind::Convert { kind: Conversion::FunctionDecay, operand } =
                    self.tast[callee].kind
                else {
                    return false;
                };
                let ExprKind::Decl(decl) = self.tast[operand].kind else { return false };
                self.tast[decl].flags.contains(DeclFlags::NORETURN)
            }
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
                // The left side is not known, so the right side runs only on the path where the
                // left side does not end the chain. If the right side has effects of its own,
                // listing them after the left side would run them on every path, and `a || (g--
                // || 1)` would decrement `g` even when `a` is true. Only a right side with
                // nothing left to run, whose answer is the one that ends the chain, lets the
                // whole condition be decided with the left side as its only effect.
                let (rest, answer) = self.condition(rhs)?;
                if answer != ends || !rest.is_empty() {
                    return None;
                }
                Some((vec![lhs], ends))
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
    /// drop what it reported on the floor. Nor does one that leaned on a `__builtin_constant_p`
    /// the optimizer has still to answer, since the folder's no is only right where a constant is
    /// needed, and `execute/builtin-constant.c` checks the arm a `?:` takes on it at `-O2`.
    fn folded(&self, cond: ExprId) -> Option<bool> {
        let mut eval = Eval::new(self.tast, self.types, self.target, self.names);
        let folded = eval.constant(cond);
        if eval.addressed() || eval.deferred() || !eval.finish().is_empty() {
            return None;
        }
        match folded {
            Ok(Const::Int(value)) => Some(value != 0),
            Ok(Const::Float(value)) => Some(!value.is_zero()),
            _ => None,
        }
    }
}
