//! The IR as something a rule can match against.
//!
//! Design: `spec/10-backend.md` section 10.2 and `spec/optimizer/13-rewrite-rules.md`.
//!
//! Two rule sets are matched against the IR. `rucc-codegen` lowers it to machine terms and
//! `rucc-opt` rewrites it to more IR, and both of them are asking what an instruction is called
//! and what its operands are. This is here rather than in either of them so that there is one
//! answer to that: a rewrite rule and a lowering rule that spelled `add.i32` differently would
//! be two vocabularies over one IR, and the day they drifted apart nothing would say so.
//!
//! A rule is written about a term and the compiler has no terms. It has a function full of
//! instructions, and what a pattern is about is one of them together with whatever its operands
//! were computed from. So this is the [`Subject`] the matcher asks its three questions of, and
//! the answers come out of the IR: nothing is built and nothing is thrown away.
//!
//! # How an operand is shown
//!
//! The same IR value can be several different terms. `(add.i32 (value.i32 x) (iconst.i32 k))`
//! and `(add.i32 (value.i32 x) (value.i32 y))` are two patterns over one instruction, and which
//! one it is depends on whether the second operand is a constant and on whether the rule that
//! wants a constant will take this one. `(add.i64 (value.i64 x) (mul.i64 (value.i64 y)
//! (iconst.i64 4)))` is a third, and it is about two instructions rather than one.
//!
//! The matcher does not backtrack across alternatives for one node: [`Subject::head`] gives one
//! answer and the walk believes it. So the choice is made before the walk rather than during it.
//! A [`Plan`] says how each operand of the instruction is shown, the caller tries the plans in
//! order, and the first that matches is the one that fires. There are at most three ways to show
//! an operand and at most two operands in any pattern either rule set has, so the whole of the
//! search is a handful of walks over a trie, each of which fails in its first node or two.
//!
//! # How deep it goes
//!
//! One level. An operand may be shown as the instruction that computed it, and that
//! instruction's own operands are shown as a register or as a constant and never expanded
//! again, which is as deep as any pattern in either rule file reaches. A rule set that wants
//! three levels needs this to grow a level, and it would be found by the rule failing to fire
//! rather than by anything going wrong.

use rucc_base::rules::Subject;

use crate::{Def, Extra, Float, FloatPred, Func, Inst, IntPred, Opcode, Type, Value};

/// How many operands of one instruction a plan can speak about.
///
/// Two is what every pattern in the rule set needs, and a third costs nothing to carry. An
/// instruction with more operands than this is one no rule matches, which is the same answer it
/// would get from a plan that could describe it.
pub const MAX_ARGS: usize = 3;

/// How one operand is shown to the matcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    /// As a value sitting in a register, which is what `(value.iN x)` matches.
    Reg,
    /// As a constant the caller has in hand, which is what `(iconst.iN k)` matches.
    Const,
    /// As a register that is not a constant, which is what `(value.iN x)` matches when the
    /// operand is anything other than a number.
    ///
    /// This is [`Shown::Reg`] with the constants refused. A canonicalisation is a rule that
    /// moves an operand from one side to the other, and the swapped form it writes matches the
    /// rule again the moment the other side is a constant too, which is a term the pass would
    /// rewrite until it ran out of fuel. Saying which side is not a number is what stops it, and
    /// it has to be said in the plan rather than in a guard, because a guard reads a binding as
    /// a number and is false when it is not one.
    Var,
    /// As the instruction that computed it, so a rule can be about two instructions at once.
    Expand,
}

/// How every operand of one instruction is shown.
pub type Plan = [Shown; MAX_ARGS];

/// Everything shown as a register, which is the plan that matches when no other does.
pub const PLAIN: Plan = [Shown::Reg; MAX_ARGS];

/// One node of the term the matcher is walking.
///
/// A position rather than a term, because the term does not exist. Two of these are values in
/// their own right, and they are the two a pattern can bind: the register a `value` wraps and
/// the number an `iconst` wraps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Term {
    /// The instruction being matched.
    Root,
    /// Operand `i` of the root, shown the way the plan says to show it.
    Arg(u8),
    /// Operand `j` of the instruction that computed operand `i` of the root.
    Deep(u8, u8),
    /// A value in a register, which is what a pattern binds when it writes `(value.iN x)`.
    Reg(Value),
    /// A constant, which is what a pattern binds or tests inside an `(iconst.iN k)`.
    Num(i128),
}

/// One instruction of a function, as the terms a rule could match.
#[derive(Debug)]
pub struct Terms<'a> {
    func: &'a Func,
    root: Inst,
    plan: Plan,
}

impl<'a> Terms<'a> {
    /// The instruction, shown the way the plan says.
    #[must_use]
    pub fn new(func: &'a Func, root: Inst, plan: Plan) -> Self {
        Self { func, root, plan }
    }

    /// The instruction this is about.
    #[must_use]
    pub fn root(&self) -> Inst {
        self.root
    }

    /// What the root, or an instruction one of its operands was expanded into, is called in a
    /// rule file.
    #[must_use]
    pub fn name(&self, inst: Inst) -> Option<&'static str> {
        head_of(self.func, inst)
    }

    /// The value operands of an instruction.
    fn args(&self, inst: Inst) -> &[Value] {
        &self.func[self.func[inst].args]
    }

    /// Operand `index` of the root, or nothing if it has no such operand.
    fn arg_value(&self, index: u8) -> Option<Value> {
        self.args(self.root).get(usize::from(index)).copied()
    }

    /// The instruction a value is the result of, or nothing for a block parameter.
    fn def_of(&self, value: Value) -> Option<Inst> {
        match self.func[value].def {
            Def::Result { inst, .. } => Some(inst),
            Def::Param { .. } => None,
        }
    }

    /// What a value is, if it is a constant.
    #[must_use]
    pub fn constant(&self, value: Value) -> Option<i128> {
        let inst = self.def_of(value)?;
        let data = &self.func[inst];
        if data.opcode != Opcode::IConst {
            return None;
        }
        let Extra::Imm(imm) = data.extra else { return None };
        let ty = self.func[value].ty;
        if !ty.is_int() {
            return None;
        }
        // One bit is read unsigned, and every other width is read signed. The sign bit of a one
        // bit integer is the whole of it, so the signed reading of a true is minus one, and what
        // a rule at that width means by the number it matched is the truth value rather than a
        // bit pattern. Reading it signed would put a byte of ones in a register where the rest of
        // the rule set expects a zero or a one.
        if is_bit(ty) {
            return Some(i128::try_from(self.func[imm].unsigned()).unwrap_or(0));
        }
        Some(self.func[imm].signed(ty))
    }

    /// The head of a value shown as a register or as a constant, which is a term of one
    /// argument either way: the thing the pattern binds.
    fn leaf_head(&self, value: Value, shown: Shown) -> Option<(&'static str, usize)> {
        let ty = self.func[value].ty;
        let name = match shown {
            Shown::Reg => value_head(ty)?,
            Shown::Const => iconst_head(ty)?,
            // A constant shown this way is not shown at all. The head is the only place that can
            // refuse it, since a binding says nothing about what the operand was called.
            Shown::Var if self.constant(value).is_none() => value_head(ty)?,
            Shown::Var => return None,
            // An expansion is not a leaf, and nothing asks this about one.
            Shown::Expand => return None,
        };
        Some((name, 1))
    }

    /// What a value shown as a register or as a constant binds, which is the value itself or
    /// the number it is.
    fn leaf_arg(&self, value: Value, shown: Shown) -> Term {
        match shown {
            Shown::Const => self.constant(value).map_or(Term::Reg(value), Term::Num),
            Shown::Reg | Shown::Var | Shown::Expand => Term::Reg(value),
        }
    }

    /// How an operand of an expanded operand is shown, which is as a constant when it is one
    /// and as a register otherwise.
    ///
    /// There is no choice to make here. The reason to show a constant as a register is that no
    /// rule would take it as an immediate, and the answer to that inside an expansion is to
    /// stop expanding, which is a plan the selector tries anyway.
    fn deep_shown(&self, value: Value) -> Shown {
        if self.constant(value).is_some() { Shown::Const } else { Shown::Reg }
    }

    /// The value a place holds, or nothing for a place that holds a constant rather than a
    /// value.
    ///
    /// This is what makes two places comparable. A rule that writes one name twice is asking
    /// whether both of its operands are the same value, and the two places are operand zero and
    /// operand one, which are never equal as places.
    fn value_at(&self, node: Term) -> Option<Value> {
        match node {
            Term::Root => self.func[self.root].first_result,
            Term::Arg(index) => self.arg_value(index),
            Term::Deep(outer, inner) => {
                self.expansion(outer).and_then(|(_, args)| args.get(usize::from(inner)).copied())
            }
            Term::Reg(value) => Some(value),
            Term::Num(_) => None,
        }
    }

    /// The instruction an expanded operand of the root was computed by, with its operands.
    fn expansion(&self, index: u8) -> Option<(Inst, &[Value])> {
        let value = self.arg_value(index)?;
        let inst = self.def_of(value)?;
        Some((inst, self.args(inst)))
    }
}

impl Subject for Terms<'_> {
    type Node = Term;

    fn head(&self, node: Term) -> Option<(&str, usize)> {
        match node {
            Term::Root => {
                let name = head_of(self.func, self.root)?;
                let data = &self.func[self.root];
                // A constant has no operands and its term has one, which is the constant, so it
                // is the one instruction whose arity is not the length of its operand list.
                let arity =
                    if data.opcode == Opcode::IConst { 1 } else { self.args(self.root).len() };
                Some((name, arity))
            }
            Term::Arg(index) => {
                let value = self.arg_value(index)?;
                match self.plan[usize::from(index)] {
                    Shown::Expand => {
                        let (inst, args) = self.expansion(index)?;
                        Some((head_of(self.func, inst)?, args.len()))
                    }
                    shown => self.leaf_head(value, shown),
                }
            }
            Term::Deep(outer, inner) => {
                let (_, args) = self.expansion(outer)?;
                let value = *args.get(usize::from(inner))?;
                self.leaf_head(value, self.deep_shown(value))
            }
            Term::Reg(_) | Term::Num(_) => None,
        }
    }

    fn arg(&self, node: Term, index: usize) -> Term {
        let index = u8::try_from(index).unwrap_or(u8::MAX);
        match node {
            Term::Root => {
                let data = &self.func[self.root];
                if data.opcode == Opcode::IConst {
                    let value = data.first_result.expect("a constant has a result");
                    return self.leaf_arg(value, Shown::Const);
                }
                Term::Arg(index)
            }
            Term::Arg(outer) => match self.plan[usize::from(outer)] {
                Shown::Expand => Term::Deep(outer, index),
                shown => {
                    self.arg_value(outer).map_or(Term::Num(0), |value| self.leaf_arg(value, shown))
                }
            },
            Term::Deep(outer, inner) => {
                let value = self
                    .expansion(outer)
                    .and_then(|(_, args)| args.get(usize::from(inner)).copied());
                value.map_or(Term::Num(0), |value| self.leaf_arg(value, self.deep_shown(value)))
            }
            // Neither has a head, so nothing asks either of them for an argument.
            Term::Reg(_) | Term::Num(_) => node,
        }
    }

    fn int(&self, node: Term) -> Option<i128> {
        match node {
            Term::Num(value) => Some(value),
            _ => None,
        }
    }

    fn same(&self, a: Term, b: Term) -> bool {
        match (self.value_at(a), self.value_at(b)) {
            (Some(left), Some(right)) => left == right,
            // Neither is a value, so the only other thing either can be is a constant the plan
            // asked to be shown as one. Two constants of the same number are the same term
            // whatever computed them, which is the one case where this is not an identity.
            _ => match (self.int(a), self.int(b)) {
                (Some(left), Some(right)) => left == right,
                _ => false,
            },
        }
    }
}

/// What an instruction is called in a rule file, or nothing if the rules have no name for it.
///
/// The one function here that a caller with an [`Inst`] and no [`Terms`] wants, which is
/// anything reporting on a rule rather than matching one.
///
/// The name carries the width, because a rule file that did not say how wide a term is would be
/// a file whose reader has to look at the line above to find out. Which widths there are names
/// for is the rule language's business and not this crate's: an instruction at a width nothing
/// is written about has no name here, and the answer to it is that no rule matches.
pub fn head_of(func: &Func, inst: Inst) -> Option<&'static str> {
    let data = &func[inst];

    // A store is the one instruction with a name here that computes nothing, so the width in
    // its name is the width of what it is storing and has to come from an operand. That operand
    // is the first one, which is the order `crate::Builder::store` puts them in and the order
    // a pattern for one is written in.
    //
    // Nothing looks at the flags or the ordering, and both of those are worth saying out loud.
    // A `volatile` access has to happen exactly once and must not move, and neither of those is
    // something selection does: one IR load is one instruction whatever its flags say, and
    // folding the address arithmetic into the addressing mode does not change how many times
    // memory is touched. An ordering would be a different matter, because a store that releases
    // is not a plain `mov` on any machine where it means anything, but an ordered access is
    // `atomic_load` or `atomic_store` and those are different opcodes with no name here. The IR
    // verifier is what makes that true rather than merely usual: it rejects an ordering on a
    // plain access, so by the time anything is selected there is none to miss.
    if data.opcode == Opcode::Store {
        let value = *func[data.args].first()?;
        return store_head(func[value].ty);
    }

    // A return is the other one, and the width comes from the operand for the same reason. A
    // return of nothing has no name, and neither has a return of more than one value: a rule
    // for either would have to say where each of them goes, and where a value goes is a fact
    // about the convention rather than about a term, so the rule language has nothing to say
    // about it. A return of nothing needs no rule at all, since the epilogue is the whole of it.
    if data.opcode == Opcode::Return {
        let [value] = &func[data.args] else { return None };
        return ret_head(func[*value].ty);
    }

    // A conditional branch is the third instruction here that computes nothing. Where it goes is
    // not part of its name and not part of any pattern: a machine IR block holds its own
    // successors, so a rule for a branch never has to say a block, and what is left for it to say
    // is what the branch is about, which is the condition.
    if data.opcode == Opcode::BrIf {
        let [cond] = &func[data.args] else { return None };
        return (func[*cond].ty == Type::int(1)).then_some(BRIF);
    }

    let result = data.first_result?;
    let ty = func[result].ty;
    match data.opcode {
        Opcode::IConst => iconst_head(ty),
        Opcode::Load => load_head(ty),
        Opcode::ICmp => {
            let Extra::IntPred(pred) = data.extra else { return None };
            Some(icmp_head(pred))
        }
        // A float comparison, whose name comes from the operands rather than from the result: the
        // result is one bit either way and what tells the two instructions apart is the format.
        Opcode::FCmp => {
            let Extra::FloatPred(pred) = data.extra else { return None };
            fcmp_head(pred, func[*func[data.args].first()?].ty)
        }
        Opcode::SExt | Opcode::ZExt | Opcode::Trunc => {
            let from = func[*func[data.args].first()?].ty;
            convert_head(data.opcode, from, ty)
        }
        // The conversions with a float on one side or both. A separate row because what is on
        // each side is part of the name and a width alone would not say which register file the
        // value is in, which is the whole difference between these and the three above.
        Opcode::FPExt | Opcode::FPTrunc | Opcode::FPToSI | Opcode::SIToFP | Opcode::Bitcast => {
            let from = func[*func[data.args].first()?].ty;
            cross_head(data.opcode, from, ty)
        }
        // Address arithmetic is an add at the address width, which is all it is once both
        // operands are in registers: the offset is already in bytes, which the IR guarantees and
        // the front end is what did the multiplying. Calling it that is what lets every rule
        // written about an add reach it, including the ones that fold it into an addressing mode,
        // and there is nothing in any of them it could get wrong.
        Opcode::PtrAdd => binary_head(Opcode::Add, ty),
        opcode => binary_head(opcode, ty),
    }
}

/// What a conditional branch is called, which carries the width of the condition and nothing
/// else, since where the branch goes is on the block rather than in the term.
///
/// A constant rather than a literal in [`head_of`] because [`heads`] says it too, and a name
/// written in two places is a name that can differ in one of them.
const BRIF: &str = "brif.i1";

/// Every name this module can give an instruction, with the opcode it gives it to.
///
/// This is what a rule file could be written about, so that the back end's coverage check can ask what one
/// is written about and say where the difference is. It comes out of the same functions
/// [`head_of`] asks rather than out of a list, because a list of names checked against another
/// list of names is a test that both were typed the same way, which is not the question worth
/// asking.
///
/// The sweep is over every type the compiler has, including the ones nothing here has a name for.
/// A width with no name contributes nothing and costs nothing, and the day one of them gets a name
/// it appears here without anybody remembering to add it, which is the property that makes this
/// worth generating rather than writing down.
pub fn heads() -> Vec<(Opcode, &'static str)> {
    let types = [
        Type::int(1),
        Type::int(8),
        Type::int(16),
        Type::int(32),
        Type::int(64),
        Type::int(128),
        Type::PTR,
        Type::float(Float::F32),
        Type::float(Float::F64),
        Type::float(Float::F80),
        Type::vector(Type::int(32), 4),
    ];

    let mut found = Vec::new();
    for opcode in Opcode::all() {
        // The names that come from one type, which is the result's for most of these and an
        // operand's for the two that compute nothing. The arms are the ones `head_of` has, in the
        // order it has them, so that a name reachable there is reachable here.
        for &ty in &types {
            let name = match opcode {
                Opcode::Store => store_head(ty),
                Opcode::Return => ret_head(ty),
                Opcode::IConst => iconst_head(ty),
                Opcode::Load => load_head(ty),
                Opcode::PtrAdd => binary_head(Opcode::Add, ty),
                _ => binary_head(opcode, ty),
            };
            if let Some(name) = name {
                found.push((opcode, name));
            }
        }
        // And the names that come from a predicate or from two types at once.
        match opcode {
            Opcode::BrIf => found.push((opcode, BRIF)),
            Opcode::ICmp => found.extend(IntPred::all().map(|pred| (opcode, icmp_head(pred)))),
            Opcode::FCmp => {
                for pred in FloatPred::all() {
                    let named = types.iter().filter_map(|&ty| fcmp_head(pred, ty));
                    found.extend(named.map(|name| (opcode, name)));
                }
            }
            Opcode::SExt | Opcode::ZExt | Opcode::Trunc => {
                for &from in &types {
                    let named = types.iter().filter_map(|&to| convert_head(opcode, from, to));
                    found.extend(named.map(|name| (opcode, name)));
                }
            }
            Opcode::FPExt | Opcode::FPTrunc | Opcode::FPToSI | Opcode::SIToFP | Opcode::Bitcast => {
                for &from in &types {
                    let named = types.iter().filter_map(|&to| cross_head(opcode, from, to));
                    found.extend(named.map(|name| (opcode, name)));
                }
            }
            _ => {}
        }
    }

    found.sort_unstable();
    found.dedup();
    found
}

/// How wide an address is on the machine this lowers for.
///
/// The rule set has no term for a pointer and needs none. An address in a register is an integer
/// of the machine's address width, every rule that could compute one is a rule about an integer
/// of that width, and the only thing missing was a name. [`slot`] used to ask the type how wide
/// it was, and a pointer answers nothing, because how wide an address is belongs to the target
/// rather than to the IR. So this is where the target's answer is written down.
///
/// Sixty four, and a constant rather than something asked of a target, because every
/// architecture `rucc_target::Arch` names is a sixty four bit one. There is no target in the
/// compiler that would want a different number, and a thirty two bit one would want more from
/// the rule sets than a number.
pub const ADDRESS: u32 = 64;

/// Which of the four widths a type is, or nothing for a width no rule is written at.
///
/// A pointer is one of them, at [`ADDRESS`]. A vector is none of them however wide its lane is,
/// because a rule at a width says nothing about how many lanes it acts on and lowering an add of
/// four lanes to an add of one would be wrong rather than incomplete.
pub fn slot(ty: Type) -> Option<usize> {
    if !ty.is_scalar() {
        return None;
    }
    let bits = if ty.is_ptr() { ADDRESS } else { ty.is_int().then(|| ty.bits())? };
    match bits {
        8 => Some(0),
        16 => Some(1),
        32 => Some(2),
        64 => Some(3),
        _ => None,
    }
}

/// Which of the two float widths a type is, or nothing for anything that is not a float.
///
/// Two rather than [`slot`]'s four, and a table of its own rather than more entries in that one,
/// because a `float` and an `int` of the same width are not the same term to any rule: they are in
/// different register files and every instruction that touches them is a different instruction. A
/// `long double` is none of them, since it is on the x87 stack rather than in a vector register
/// and nothing here is written about that stack.
pub fn float_slot(ty: Type) -> Option<usize> {
    if !ty.is_scalar() || !ty.is_float() {
        return None;
    }
    match ty.bits() {
        32 => Some(0),
        64 => Some(1),
        _ => None,
    }
}

/// Whether a type is the one bit a truth value comes in.
///
/// One bit is a width the rule set is written at and is not one of [`slot`]'s four, because it is
/// not a width the machine computes in. There is no one bit register and no one bit instruction: a
/// value of this width lives in a whole byte with the other seven bits zero, which is what a
/// `setcc` leaves behind, and every rule written at one bit is a byte instruction chosen because
/// it keeps that true. The model says the same thing from the other side, giving `setcc` a meaning
/// one bit wide, so the abstraction is stated in both places rather than assumed in either.
///
/// What makes the invariant hold rather than merely be usual is that nothing else at this width
/// has a name. A comparison is the only instruction that produces one, the bitwise operations
/// below carry it through unchanged, and everything else at one bit reaches [`slot`] and gets
/// nothing, so there is no rule that could put a byte here which is not a zero or a one.
fn is_bit(ty: Type) -> bool {
    ty.is_scalar() && ty.is_int() && ty.bits() == 1
}

/// What a value in a register is called at that width.
fn value_head(ty: Type) -> Option<&'static str> {
    if is_bit(ty) {
        return Some("value.i1");
    }
    if let Some(at) = float_slot(ty) {
        return Some(["value.f32", "value.f64"][at]);
    }
    Some(["value.i8", "value.i16", "value.i32", "value.i64"][slot(ty)?])
}

/// What a constant is called at that width.
///
/// An integer and not an address, unlike everything else here. What a pattern binds inside one of
/// these is the number, and [`Terms::constant`] only has a number for an integer, so a term that
/// named an address would be one a rule could match and then find nothing behind.
fn iconst_head(ty: Type) -> Option<&'static str> {
    if !ty.is_int() {
        return None;
    }
    if is_bit(ty) {
        return Some("iconst.i1");
    }
    Some(["iconst.i8", "iconst.i16", "iconst.i32", "iconst.i64"][slot(ty)?])
}

/// What a load is called, which is the width of the value it produced.
fn load_head(ty: Type) -> Option<&'static str> {
    if let Some(at) = float_slot(ty) {
        return Some(["load.f32", "load.f64"][at]);
    }
    Some(["load.i8", "load.i16", "load.i32", "load.i64"][slot(ty)?])
}

/// What a store is called, which is the width of the value it writes, since it produces nothing
/// to take a width from.
fn store_head(ty: Type) -> Option<&'static str> {
    if let Some(at) = float_slot(ty) {
        return Some(["store.f32", "store.f64"][at]);
    }
    Some(["store.i8", "store.i16", "store.i32", "store.i64"][slot(ty)?])
}

/// What a return is called, which is the width of the value it gives back, for the same reason.
fn ret_head(ty: Type) -> Option<&'static str> {
    if let Some(at) = float_slot(ty) {
        return Some(["ret.f32", "ret.f64"][at]);
    }
    Some(["ret.i8", "ret.i16", "ret.i32", "ret.i64"][slot(ty)?])
}

/// What a comparison is called, which does not carry the width of what it compared: the result
/// is one bit whatever the operands were, and the operands say how wide they are themselves.
fn icmp_head(pred: IntPred) -> &'static str {
    match pred {
        IntPred::Eq => "icmp_eq.i1",
        IntPred::Ne => "icmp_ne.i1",
        IntPred::Slt => "icmp_slt.i1",
        IntPred::Sle => "icmp_sle.i1",
        IntPred::Sgt => "icmp_sgt.i1",
        IntPred::Sge => "icmp_sge.i1",
        IntPred::Ult => "icmp_ult.i1",
        IntPred::Ule => "icmp_ule.i1",
        IntPred::Ugt => "icmp_ugt.i1",
        IntPred::Uge => "icmp_uge.i1",
    }
}

/// The predicate a head names, when the head is a comparison of two integers.
///
/// The inverse of `icmp_head`, which is private, and a search over it rather than a second table, because two
/// tables that are supposed to be inverses are two tables that will stop being inverses. Ten
/// comparisons is a short enough search that the alternative would be arranging for a map to be
/// built once, and this is asked once per rule that fires rather than once per instruction.
///
/// What wants this is the peephole. A rule may write a comparison, and the predicate is not part
/// of the opcode: [`heads`] gives every predicate the same [`Opcode::ICmp`], so a rewriter that
/// asked only for the opcode would build a comparison with whatever predicate happened to be on
/// the instruction it replaced. That is not an instruction computing something else, it is one
/// computing the opposite.
pub fn int_pred(head: &str) -> Option<IntPred> {
    IntPred::all().find(|&pred| icmp_head(pred) == head)
}

/// What a float comparison is called, which does carry the format of what it compared.
///
/// The difference from [`icmp_head`] is the whole reason this is a second function. A comparison
/// of two integers is the same instruction whatever file they came from, because there is only one
/// file they could have come from, so the width lives on the operands and the name says nothing
/// about it. A comparison of two floats is a different instruction for a `float` and a `double`,
/// and the operands are in registers that hold either, so the name has to say which.
///
/// The two predicates that read nothing have no name here. `false` and `true` do not look at their
/// operands, so a rule for either would be a rule that computes a constant out of a comparison it
/// did not make, and the front end writes neither: nothing in C spells them and nothing here folds
/// a comparison into one yet.
fn fcmp_head(pred: FloatPred, ty: Type) -> Option<&'static str> {
    let at = float_slot(ty)?;
    let names: [&'static str; 2] = match pred {
        FloatPred::Oeq => ["fcmp_oeq.f32.i1", "fcmp_oeq.f64.i1"],
        FloatPred::Ogt => ["fcmp_ogt.f32.i1", "fcmp_ogt.f64.i1"],
        FloatPred::Oge => ["fcmp_oge.f32.i1", "fcmp_oge.f64.i1"],
        FloatPred::Olt => ["fcmp_olt.f32.i1", "fcmp_olt.f64.i1"],
        FloatPred::Ole => ["fcmp_ole.f32.i1", "fcmp_ole.f64.i1"],
        FloatPred::One => ["fcmp_one.f32.i1", "fcmp_one.f64.i1"],
        FloatPred::Ord => ["fcmp_ord.f32.i1", "fcmp_ord.f64.i1"],
        FloatPred::Uno => ["fcmp_uno.f32.i1", "fcmp_uno.f64.i1"],
        FloatPred::Ueq => ["fcmp_ueq.f32.i1", "fcmp_ueq.f64.i1"],
        FloatPred::Ugt => ["fcmp_ugt.f32.i1", "fcmp_ugt.f64.i1"],
        FloatPred::Uge => ["fcmp_uge.f32.i1", "fcmp_uge.f64.i1"],
        FloatPred::Ult => ["fcmp_ult.f32.i1", "fcmp_ult.f64.i1"],
        FloatPred::Ule => ["fcmp_ule.f32.i1", "fcmp_ule.f64.i1"],
        FloatPred::Une => ["fcmp_une.f32.i1", "fcmp_une.f64.i1"],
        FloatPred::False | FloatPred::True => return None,
    };
    Some(names[at])
}

/// What a conversion is called, which is the two widths it is between.
///
/// A widening from one bit is the one conversion this width has, and it is a row of its own rather
/// than a fifth entry in the tables below. A five by five table would have a name for every
/// conversion between one bit and every other width in both directions, and all but four of those
/// are conversions nothing writes: a narrowing to one bit is a comparison against zero, which is a
/// different opcode, and a sign extension from one bit is what an `unsigned` comparison result
/// would need and there is none.
fn convert_head(opcode: Opcode, from: Type, to: Type) -> Option<&'static str> {
    if is_bit(from) {
        if opcode != Opcode::ZExt {
            return None;
        }
        return Some(["zext.i1.i8", "zext.i1.i16", "zext.i1.i32", "zext.i1.i64"][slot(to)?]);
    }
    let table: &[[Option<&'static str>; 4]; 4] = match opcode {
        Opcode::SExt => &SEXT,
        Opcode::ZExt => &ZEXT,
        Opcode::Trunc => &TRUNC,
        _ => return None,
    };
    table[slot(from)?][slot(to)?]
}

/// Which of the two integer widths a conversion to or from a float is written at, or nothing for
/// any other width.
///
/// The machine converts at thirty two bits and at sixty four and at no width below them. A C
/// program turning a `double` into a `short` is a conversion to `int` and a truncation after it,
/// and the front end is what writes the truncation, so a narrower conversion arriving here has no
/// name and is reported rather than lowered to an instruction that would round it in the wrong
/// place.
fn cross_slot(ty: Type) -> Option<usize> {
    match slot(ty)? {
        2 => Some(0),
        3 => Some(1),
        _ => None,
    }
}

/// Whether that type is the integer the float at that index shares its width with.
///
/// A pointer is not, however wide it is. The IR has `ptrtoint` for turning an address into a
/// number, and a `bitcast` that moved one through a vector register would be hiding that
/// conversion rather than performing it, which is what the IR verifier says as well.
fn paired_int(ty: Type, at: usize) -> bool {
    ty.is_scalar() && ty.is_int() && ty.bits() == [32, 64][at]
}

/// What a conversion with a float on one side or both is called, which is what it goes between and
/// which side each of them is on.
///
/// The name carries the format where an integer conversion carries a width, for the reason
/// [`float_slot`] gives: a `float` and an `int` of the same width are in different register files
/// and no rule written about one says anything about the other. So there is no name here that
/// could be read as either, and a rule for `fptosi.f64.i32` cannot match anything but a `double`
/// becoming an `int`.
///
/// The unsigned conversions have no name. The machine has no instruction for either below a
/// register wider than anything this allocates, so each is several instructions and belongs in a
/// pass that rewrites it into these rather than in a rule that would have to be several
/// instructions long.
fn cross_head(opcode: Opcode, from: Type, to: Type) -> Option<&'static str> {
    match opcode {
        // Between the two formats, one name each way. There is no third format with a name here,
        // so these two are the whole of it rather than the first two of a table.
        Opcode::FPExt => {
            (float_slot(from)? == 0 && float_slot(to)? == 1).then_some("fpext.f32.f64")
        }
        Opcode::FPTrunc => {
            (float_slot(from)? == 1 && float_slot(to)? == 0).then_some("fptrunc.f64.f32")
        }
        Opcode::FPToSI => Some(FPTOSI[float_slot(from)?][cross_slot(to)?]),
        Opcode::SIToFP => Some(SITOFP[cross_slot(from)?][float_slot(to)?]),
        // A reinterpretation, which is a `movd` or a `movq` between the two register files and is
        // the one conversion here that changes no bit. Between two integers or between two floats
        // it is nothing at all, since the IR keeps the width the same, so the four that cross the
        // files are the four with a name.
        Opcode::Bitcast => match (float_slot(from), float_slot(to)) {
            (Some(at), None) if paired_int(to, at) => {
                Some(["bitcast.f32.i32", "bitcast.f64.i64"][at])
            }
            (None, Some(at)) if paired_int(from, at) => {
                Some(["bitcast.i32.f32", "bitcast.i64.f64"][at])
            }
            _ => None,
        },
        _ => None,
    }
}

/// A float to a signed integer, from the format down the side to the width across the top.
static FPTOSI: [[&str; 2]; 2] =
    [["fptosi.f32.i32", "fptosi.f32.i64"], ["fptosi.f64.i32", "fptosi.f64.i64"]];

/// A signed integer to a float, the other way round.
static SITOFP: [[&str; 2]; 2] =
    [["sitofp.i32.f32", "sitofp.i32.f64"], ["sitofp.i64.f32", "sitofp.i64.f64"]];

/// What each of the binary operations is called at each width.
///
/// The three bitwise ones are the only ones with a name at one bit. They are what a `!=` between
/// two truth values and a `&&` folded to one instruction become, and each of them takes two bytes
/// that are a zero or a one to a byte that is a zero or a one. There is nothing to be gained by an
/// add or a shift at this width and no front end writes one.
fn binary_head(opcode: Opcode, ty: Type) -> Option<&'static str> {
    if is_bit(ty) {
        return match opcode {
            Opcode::And => Some("and.i1"),
            Opcode::Or => Some("or.i1"),
            Opcode::Xor => Some("xor.i1"),
            _ => None,
        };
    }
    if let Some(at) = float_slot(ty) {
        // The four the machine has one instruction each for. A remainder is not among them: there
        // is no scalar instruction for it and what C means by `fmod` is a call, so an `frem` that
        // reached here would find no rule and be reported rather than lowered to something else.
        let names: &[&'static str; 2] = match opcode {
            Opcode::FAdd => &["fadd.f32", "fadd.f64"],
            Opcode::FSub => &["fsub.f32", "fsub.f64"],
            Opcode::FMul => &["fmul.f32", "fmul.f64"],
            Opcode::FDiv => &["fdiv.f32", "fdiv.f64"],
            _ => return None,
        };
        return Some(names[at]);
    }
    let names: &[&'static str; 4] = match opcode {
        Opcode::Add => &["add.i8", "add.i16", "add.i32", "add.i64"],
        Opcode::Sub => &["sub.i8", "sub.i16", "sub.i32", "sub.i64"],
        Opcode::Mul => &["mul.i8", "mul.i16", "mul.i32", "mul.i64"],
        Opcode::SDiv => &["sdiv.i8", "sdiv.i16", "sdiv.i32", "sdiv.i64"],
        Opcode::UDiv => &["udiv.i8", "udiv.i16", "udiv.i32", "udiv.i64"],
        Opcode::SRem => &["srem.i8", "srem.i16", "srem.i32", "srem.i64"],
        Opcode::URem => &["urem.i8", "urem.i16", "urem.i32", "urem.i64"],
        Opcode::And => &["and.i8", "and.i16", "and.i32", "and.i64"],
        Opcode::Or => &["or.i8", "or.i16", "or.i32", "or.i64"],
        Opcode::Xor => &["xor.i8", "xor.i16", "xor.i32", "xor.i64"],
        Opcode::Shl => &["shl.i8", "shl.i16", "shl.i32", "shl.i64"],
        Opcode::LShr => &["lshr.i8", "lshr.i16", "lshr.i32", "lshr.i64"],
        Opcode::AShr => &["ashr.i8", "ashr.i16", "ashr.i32", "ashr.i64"],
        _ => return None,
    };
    Some(names[slot(ty)?])
}

/// The widening conversions, from the width down the side to the width across the top. The
/// diagonal and everything below it is empty, because a sign extension to a width it already
/// has is not an instruction and the IR does not have one.
static SEXT: [[Option<&str>; 4]; 4] = [
    [None, Some("sext.i8.i16"), Some("sext.i8.i32"), Some("sext.i8.i64")],
    [None, None, Some("sext.i16.i32"), Some("sext.i16.i64")],
    [None, None, None, Some("sext.i32.i64")],
    [None, None, None, None],
];

static ZEXT: [[Option<&str>; 4]; 4] = [
    [None, Some("zext.i8.i16"), Some("zext.i8.i32"), Some("zext.i8.i64")],
    [None, None, Some("zext.i16.i32"), Some("zext.i16.i64")],
    [None, None, None, Some("zext.i32.i64")],
    [None, None, None, None],
];

/// The narrowing ones, which fill the other corner for the same reason.
static TRUNC: [[Option<&str>; 4]; 4] = [
    [None, None, None, None],
    [Some("trunc.i16.i8"), None, None, None],
    [Some("trunc.i32.i8"), Some("trunc.i32.i16"), None, None],
    [Some("trunc.i64.i8"), Some("trunc.i64.i16"), Some("trunc.i64.i32"), None],
];

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;
    use crate::{Builder, Flags, Signature};

    /// A function with one block, and the builder to put instructions in it.
    fn func() -> (Func, crate::Block) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let block = func.create_block();
        (func, block)
    }

    /// The instruction that computed a value, which every value in these tests has.
    fn inst_of(func: &Func, value: Value) -> Inst {
        match func[value].def {
            Def::Result { inst, .. } => inst,
            Def::Param { .. } => unreachable!(),
        }
    }

    #[test]
    fn an_instruction_is_the_term_the_rule_file_names_it_by() {
        let (mut func, block) = func();
        let i32 = Type::int(32);
        let mut build = Builder::new(&mut func, block);
        let k = build.iconst(i32, 7);
        let x = build.iconst(i32, 3);
        let sum = build.binary(Opcode::Add, x, k, Flags::default());
        let add = inst_of(&func, sum);

        let terms = Terms::new(&func, add, PLAIN);
        assert_eq!(terms.head(Term::Root), Some(("add.i32", 2)));
        assert_eq!(terms.head(Term::Arg(0)), Some(("value.i32", 1)));
        assert_eq!(terms.arg(Term::Arg(0), 0), Term::Reg(x));
        assert_eq!(terms.head(Term::Reg(x)), None);
        assert_eq!(terms.int(Term::Reg(x)), None);
    }

    /// What a pattern writing one name in two places asks. The two operands of `x & x` are
    /// operand zero and operand one, so the question is about the values in them and not about
    /// the places, and `spec/optimizer/13-rewrite-rules.md` section 13.4 has four identities
    /// that cannot be written without it.
    #[test]
    fn two_places_are_the_same_term_when_the_same_value_is_in_both() {
        let (mut func, block) = func();
        let i32 = Type::int(32);
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(i32, 3);
        let y = build.iconst(i32, 5);
        let both = build.binary(Opcode::And, x, x, Flags::default());
        let apart = build.binary(Opcode::And, x, y, Flags::default());

        let terms = Terms::new(&func, inst_of(&func, both), PLAIN);
        let left = terms.arg(Term::Arg(0), 0);
        let right = terms.arg(Term::Arg(1), 0);
        assert_ne!(Term::Arg(0), Term::Arg(1));
        assert!(terms.same(left, right));

        let terms = Terms::new(&func, inst_of(&func, apart), PLAIN);
        let left = terms.arg(Term::Arg(0), 0);
        let right = terms.arg(Term::Arg(1), 0);
        assert!(!terms.same(left, right));
    }

    /// Two operands shown as constants are the same term when they are the same number, whatever
    /// computed each of them. That is the one case where this is not identity of a value, and it
    /// is right: a rule about `x - x` is about what the operands are, and two `3`s are one term.
    #[test]
    fn two_constants_of_one_number_are_the_same_term() {
        let (mut func, block) = func();
        let i32 = Type::int(32);
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(i32, 3);
        let y = build.iconst(i32, 3);
        let sum = build.binary(Opcode::Add, x, y, Flags::default());

        let terms = Terms::new(&func, inst_of(&func, sum), [Shown::Const; MAX_ARGS]);
        let left = terms.arg(Term::Arg(0), 0);
        let right = terms.arg(Term::Arg(1), 0);
        assert_ne!(x, y);
        assert_eq!((left, right), (Term::Num(3), Term::Num(3)));
        assert!(terms.same(left, right));
        // And a constant is not the value beside it, because one of them has a number and the
        // other has not.
        assert!(!terms.same(left, Term::Reg(y)));
    }

    #[test]
    fn an_operand_shown_as_a_constant_gives_the_number_up() {
        let (mut func, block) = func();
        let i32 = Type::int(32);
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(i32, 3);
        let k = build.iconst(i32, -7);
        let sum = build.binary(Opcode::Add, x, k, Flags::default());
        let add = inst_of(&func, sum);

        let terms = Terms::new(&func, add, [Shown::Reg, Shown::Const, Shown::Reg]);
        assert_eq!(terms.head(Term::Arg(1)), Some(("iconst.i32", 1)));
        assert_eq!(terms.arg(Term::Arg(1), 0), Term::Num(-7));
        assert_eq!(terms.int(Term::Num(-7)), Some(-7));
        // The same operand shown as a register is a register, and a guard asking what number it
        // is gets no answer, which is what makes a rule about a number decline it.
        let plain = Terms::new(&func, add, PLAIN);
        assert_eq!(plain.head(Term::Arg(1)), Some(("value.i32", 1)));
        assert_eq!(plain.int(plain.arg(Term::Arg(1), 0)), None);
    }

    /// An operand shown as a variable is a register when it is one and nothing at all when it is a
    /// number.
    ///
    /// This is the whole of what [`Shown::Var`] is for. A canonicalisation that moves a constant
    /// to the right has to be able to say that the right side does not already hold one, and the
    /// only place that can be said is the head, since the binding is a register either way.
    #[test]
    fn an_operand_shown_as_a_variable_refuses_to_be_a_constant() {
        let (mut func, block) = func();
        let i32 = Type::int(32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let k = build.iconst(i32, 3);
        let sum = build.binary(Opcode::Add, x, k, Flags::default());
        let add = inst_of(&func, sum);

        // Operand zero is the parameter, so it is shown, and it is shown as a register.
        let terms = Terms::new(&func, add, [Shown::Var, Shown::Var, Shown::Reg]);
        assert_eq!(terms.head(Term::Arg(0)), Some(("value.i32", 1)));
        assert_eq!(terms.arg(Term::Arg(0), 0), Term::Reg(x));
        // Operand one is the constant, so there is no head and no rule reaches past it. Shown as
        // a plain register it would be `value.i32` and the rule would match.
        assert_eq!(terms.head(Term::Arg(1)), None);
        assert_eq!(Terms::new(&func, add, PLAIN).head(Term::Arg(1)), Some(("value.i32", 1)));
    }

    #[test]
    fn a_constant_is_a_term_of_one_argument_and_has_no_operands() {
        let (mut func, block) = func();
        let mut build = Builder::new(&mut func, block);
        let k = build.iconst(Type::int(64), 12);
        let inst = inst_of(&func, k);

        let terms = Terms::new(&func, inst, PLAIN);
        assert_eq!(terms.head(Term::Root), Some(("iconst.i64", 1)));
        assert_eq!(terms.arg(Term::Root, 0), Term::Num(12));
    }

    #[test]
    fn an_expanded_operand_is_the_instruction_that_computed_it() {
        let (mut func, block) = func();
        let i64 = Type::int(64);
        // A parameter, because the point of the test is an operand that is not a constant.
        let y = func.append_param(block, i64);
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(i64, 1);
        let four = build.iconst(i64, 4);
        let scaled = build.binary(Opcode::Mul, y, four, Flags::default());
        let sum = build.binary(Opcode::Add, x, scaled, Flags::default());
        let add = inst_of(&func, sum);

        let terms = Terms::new(&func, add, [Shown::Reg, Shown::Expand, Shown::Reg]);
        assert_eq!(terms.head(Term::Root), Some(("add.i64", 2)));
        assert_eq!(terms.head(Term::Arg(1)), Some(("mul.i64", 2)));
        assert_eq!(terms.head(Term::Deep(1, 0)), Some(("value.i64", 1)));
        assert_eq!(terms.arg(Term::Deep(1, 0), 0), Term::Reg(y));
        // The constant inside an expansion is shown as one without being asked to be.
        assert_eq!(terms.head(Term::Deep(1, 1)), Some(("iconst.i64", 1)));
        assert_eq!(terms.arg(Term::Deep(1, 1), 0), Term::Num(4));
    }

    #[test]
    fn a_comparison_says_which_one_it_is_and_a_conversion_says_both_widths() {
        let (mut func, block) = func();
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(Type::int(32), 1);
        let y = build.iconst(Type::int(32), 2);
        let less = build.icmp(IntPred::Slt, x, y);
        let wide = build.unary(Opcode::SExt, x, Type::int(64));
        let narrow = build.unary(Opcode::Trunc, x, Type::int(8));
        let cmp = inst_of(&func, less);
        assert_eq!(Terms::new(&func, cmp, PLAIN).head(Term::Root), Some(("icmp_slt.i1", 2)));
        let sext = inst_of(&func, wide);
        assert_eq!(Terms::new(&func, sext, PLAIN).head(Term::Root), Some(("sext.i32.i64", 1)));
        let trunc = inst_of(&func, narrow);
        assert_eq!(Terms::new(&func, trunc, PLAIN).head(Term::Root), Some(("trunc.i32.i8", 1)));
    }

    #[test]
    fn a_width_no_rule_is_written_at_has_no_name() {
        let (mut func, block) = func();
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(Type::int(128), 1);
        let inst = inst_of(&func, x);
        assert_eq!(Terms::new(&func, inst, PLAIN).head(Term::Root), None);
    }

    /// An address is an integer of the machine's width to every term here, which is what lets one
    /// be loaded from, stored through, returned and added to by rules written about integers.
    #[test]
    fn an_address_is_an_integer_as_wide_as_the_machine_addresses() {
        assert_eq!(value_head(Type::PTR), Some("value.i64"));
        assert_eq!(load_head(Type::PTR), Some("load.i64"));
        assert_eq!(store_head(Type::PTR), Some("store.i64"));
        assert_eq!(ret_head(Type::PTR), Some("ret.i64"));
        // Not a constant, since nothing writes an address down as one.
        assert_eq!(iconst_head(Type::PTR), None);
    }

    /// One bit is a width with names of its own, and they are not the four the tables hold. What
    /// has a name there is what a truth value is written with: a constant, the three bitwise
    /// operations, and the widening that turns one into a number.
    #[test]
    fn one_bit_is_a_width_with_a_name_for_what_a_truth_value_is_written_with() {
        let bit = Type::int(1);
        assert_eq!(slot(bit), None);
        assert_eq!(value_head(bit), Some("value.i1"));
        assert_eq!(iconst_head(bit), Some("iconst.i1"));
        assert_eq!(binary_head(Opcode::And, bit), Some("and.i1"));
        assert_eq!(binary_head(Opcode::Or, bit), Some("or.i1"));
        assert_eq!(binary_head(Opcode::Xor, bit), Some("xor.i1"));
        assert_eq!(convert_head(Opcode::ZExt, bit, Type::int(8)), Some("zext.i1.i8"));
        assert_eq!(convert_head(Opcode::ZExt, bit, Type::int(32)), Some("zext.i1.i32"));
        assert_eq!(convert_head(Opcode::ZExt, bit, Type::int(64)), Some("zext.i1.i64"));
    }

    /// Everything else at one bit has no name, which is what keeps the byte holding one a zero or
    /// a one: an add at this width would be an instruction that leaves something else there.
    #[test]
    fn nothing_else_at_one_bit_has_a_name() {
        let bit = Type::int(1);
        assert_eq!(binary_head(Opcode::Add, bit), None);
        assert_eq!(binary_head(Opcode::Shl, bit), None);
        assert_eq!(load_head(bit), None);
        assert_eq!(store_head(bit), None);
        assert_eq!(ret_head(bit), None);
        // Not a sign extension either, which would be a truth value spread over every bit.
        assert_eq!(convert_head(Opcode::SExt, bit, Type::int(32)), None);
        // And not a narrowing to it, since what makes a number into a truth value is a
        // comparison against zero and that is a different opcode.
        assert_eq!(convert_head(Opcode::Trunc, Type::int(32), bit), None);
    }

    /// A one bit constant is the truth value it stands for. The signed reading of a one bit
    /// integer turns a true into a minus one, which would put a byte of ones where every rule at
    /// this width expects a one.
    #[test]
    fn a_one_bit_constant_is_a_zero_or_a_one_rather_than_a_zero_or_a_minus_one() {
        let (mut func, block) = func();
        let mut build = Builder::new(&mut func, block);
        let bit = Type::int(1);
        let no = build.iconst(bit, 0);
        let yes = build.iconst(bit, 1);
        let terms = Terms::new(&func, inst_of(&func, yes), PLAIN);
        assert_eq!(terms.constant(no), Some(0));
        assert_eq!(terms.constant(yes), Some(1));
        assert_eq!(terms.head(Term::Root), Some(("iconst.i1", 1)));
        assert_eq!(terms.arg(Term::Root, 0), Term::Num(1));
    }

    /// A float is a term of its own at each of the two widths the machine has instructions for.
    /// The same width of integer is a different term, which is what keeps a rule about one from
    /// ever firing on the other, and it has to be, because the two are in different register
    /// files.
    #[test]
    fn a_float_is_a_term_of_its_own_at_each_width_the_machine_computes_in() {
        let f32 = Type::float(Float::F32);
        let f64 = Type::float(Float::F64);
        assert_eq!(value_head(f32), Some("value.f32"));
        assert_eq!(value_head(f64), Some("value.f64"));
        assert_eq!(load_head(f32), Some("load.f32"));
        assert_eq!(store_head(f64), Some("store.f64"));
        assert_eq!(ret_head(f32), Some("ret.f32"));
        assert_eq!(binary_head(Opcode::FAdd, f32), Some("fadd.f32"));
        assert_eq!(binary_head(Opcode::FSub, f64), Some("fsub.f64"));
        assert_eq!(binary_head(Opcode::FMul, f32), Some("fmul.f32"));
        assert_eq!(binary_head(Opcode::FDiv, f64), Some("fdiv.f64"));
        // Not one of the four widths an integer rule is written at, and not a constant either,
        // since what a pattern binds inside an `iconst` is a number and a float is not one.
        assert_eq!(slot(f32), None);
        assert_eq!(slot(f64), None);
        assert_eq!(iconst_head(f64), None);
        // An integer add at thirty two bits is a different name from a float add at the same
        // width, which is the whole of what keeps the two rule sets apart.
        assert_ne!(binary_head(Opcode::Add, Type::int(32)), binary_head(Opcode::FAdd, f32));
    }

    /// What the machine has no scalar instruction for has no name, so it is reported rather than
    /// lowered to something near it. A remainder is a call to `fmod` and a `long double` is on the
    /// x87 stack, and neither is anything a rule in this set is written about.
    #[test]
    fn a_float_operation_the_machine_lacks_has_no_name() {
        assert_eq!(binary_head(Opcode::FRem, Type::float(Float::F32)), None);
        let long = Type::float(Float::F80);
        assert_eq!(float_slot(long), None);
        assert_eq!(value_head(long), None);
        assert_eq!(binary_head(Opcode::FAdd, long), None);
        assert_eq!(ret_head(long), None);
    }

    /// A lane count is not a width, so a rule written at a width does not get to answer for a
    /// vector of that width. Nothing produces one yet and the day something does it should be
    /// reported rather than lowered to an instruction that acts on one lane of it.
    #[test]
    fn a_vector_is_not_the_width_of_its_lane() {
        let i32x4 = Type::vector(Type::int(32), 4);
        assert_eq!(slot(i32x4), None);
        assert_eq!(value_head(i32x4), None);
        assert_eq!(binary_head(Opcode::Add, i32x4), None);
    }

    /// The sweep says the same thing about an instruction that looking the instruction up does,
    /// which is the only way it is worth anything: a list of names built beside the naming rather
    /// than out of it would be a second table to keep in step.
    #[test]
    fn the_names_the_sweep_finds_are_the_names_an_instruction_gets() {
        let (mut func, block) = func();
        let other = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let cond = build.iconst(Type::int(1), 1);
        let x = build.iconst(Type::int(32), 1);
        let sum = build.binary(Opcode::Add, x, x, Flags::default());
        let branch = build.br_if(cond, other, &[], other, &[]);

        let names = heads();
        for inst in [inst_of(&func, sum), inst_of(&func, x), branch] {
            let name = head_of(&func, inst).expect("all three have a name");
            let opcode = func[inst].opcode;
            assert!(
                names.contains(&(opcode, name)),
                "an instruction is called {name} and the sweep does not know that name"
            );
        }
    }

    /// Every name is there once and belongs to one opcode. A name in the list twice would count
    /// twice in the coverage report, and the two instructions a name could belong to are the two
    /// the machine has one instruction for: an add of two numbers and an add of an address.
    #[test]
    fn a_name_is_listed_once_and_an_address_add_is_the_one_name_two_opcodes_share() {
        let names = heads();
        let mut once = names.clone();
        once.dedup();
        assert_eq!(names, once, "the sweep lists a name twice");
        assert!(names.contains(&(Opcode::Add, "add.i64")));
        assert!(names.contains(&(Opcode::PtrAdd, "add.i64")));
    }

    /// A width nothing is written at contributes nothing, which is what makes the sweep safe to
    /// run over every type there is. These four are the widths that have no name today, and each
    /// is an issue rather than an oversight: one bit arithmetic, `__int128`, `long double` and a
    /// vector of any lane count.
    #[test]
    fn a_width_with_no_name_puts_nothing_in_the_sweep() {
        let named: Vec<&'static str> = heads().into_iter().map(|(_, name)| name).collect();
        for name in &named {
            assert!(!name.contains("i128"), "{name} is a width no rule is written at");
            assert!(!name.contains("f80"), "{name} is a width no rule is written at");
        }
        // One bit is the width with some names and not others, so it is checked from the other
        // side: what a truth value is written with, and nothing else. A comparison is in the list
        // because its result is one bit, whatever it compared.
        let mut bit: Vec<&'static str> =
            named.into_iter().filter(|name| name.ends_with(".i1")).collect();
        bit.sort_unstable();
        assert_eq!(
            bit,
            [
                "and.i1",
                "brif.i1",
                "fcmp_oeq.f32.i1",
                "fcmp_oeq.f64.i1",
                "fcmp_oge.f32.i1",
                "fcmp_oge.f64.i1",
                "fcmp_ogt.f32.i1",
                "fcmp_ogt.f64.i1",
                "fcmp_ole.f32.i1",
                "fcmp_ole.f64.i1",
                "fcmp_olt.f32.i1",
                "fcmp_olt.f64.i1",
                "fcmp_one.f32.i1",
                "fcmp_one.f64.i1",
                "fcmp_ord.f32.i1",
                "fcmp_ord.f64.i1",
                "fcmp_ueq.f32.i1",
                "fcmp_ueq.f64.i1",
                "fcmp_uge.f32.i1",
                "fcmp_uge.f64.i1",
                "fcmp_ugt.f32.i1",
                "fcmp_ugt.f64.i1",
                "fcmp_ule.f32.i1",
                "fcmp_ule.f64.i1",
                "fcmp_ult.f32.i1",
                "fcmp_ult.f64.i1",
                "fcmp_une.f32.i1",
                "fcmp_une.f64.i1",
                "fcmp_uno.f32.i1",
                "fcmp_uno.f64.i1",
                "icmp_eq.i1",
                "icmp_ne.i1",
                "icmp_sge.i1",
                "icmp_sgt.i1",
                "icmp_sle.i1",
                "icmp_slt.i1",
                "icmp_uge.i1",
                "icmp_ugt.i1",
                "icmp_ule.i1",
                "icmp_ult.i1",
                "iconst.i1",
                "or.i1",
                "xor.i1",
            ]
        );
    }

    /// Address arithmetic is named as the add it is, which is what puts it in reach of every rule
    /// written about one, including the two below that fold it into an address.
    #[test]
    fn address_arithmetic_is_an_add_at_the_address_width() {
        let (mut func, block) = func();
        let base = func.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        let step = build.iconst(Type::int(64), 4);
        let args = func.push_values(&[base, step]);
        let next = Builder::new(&mut func, block)
            .value(crate::InstData { args, ..crate::InstData::new(Opcode::PtrAdd) }, Type::PTR);
        let inst = inst_of(&func, next);

        let terms = Terms::new(&func, inst, [Shown::Reg, Shown::Const, Shown::Reg]);
        assert_eq!(terms.head(Term::Root), Some(("add.i64", 2)));
        assert_eq!(terms.head(Term::Arg(0)), Some(("value.i64", 1)));
        assert_eq!(terms.head(Term::Arg(1)), Some(("iconst.i64", 1)));
        assert_eq!(terms.arg(Term::Arg(1), 0), Term::Num(4));
    }
}
