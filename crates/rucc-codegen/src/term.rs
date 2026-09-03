//! The IR as something a lowering rule can match against.
//!
//! Design: `spec/10-backend.md` section 10.2.
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
//! A [`Plan`] says how each operand of the instruction is shown, the selector tries the plans in
//! order, and the first that matches is the one that fires. There are at most three ways to show
//! an operand and at most two operands in any pattern this rule set has, so the whole of the
//! search is a handful of walks over a trie, each of which fails in its first node or two.
//!
//! # How deep it goes
//!
//! One level. An operand may be shown as the instruction that computed it, and that
//! instruction's own operands are shown as a register or as a constant and never expanded
//! again, which is as deep as any pattern in `x86-64.rules` reaches. A rule set that wants three
//! levels needs this to grow a level, and it would be found by the rule failing to fire rather
//! than by anything going wrong.

use rucc_ir::{Def, Extra, Func, Inst, IntPred, Opcode, Type, Value};

use crate::select::Subject;

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
    /// As a constant the selector has in hand, which is what `(iconst.iN k)` matches.
    Const,
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
    /// The instruction being selected.
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
        ty.is_int().then(|| self.func[imm].signed(ty))
    }

    /// The head of a value shown as a register or as a constant, which is a term of one
    /// argument either way: the thing the pattern binds.
    fn leaf_head(&self, value: Value, shown: Shown) -> Option<(&'static str, usize)> {
        let ty = self.func[value].ty;
        let name = match shown {
            Shown::Reg => value_head(ty)?,
            Shown::Const => iconst_head(ty)?,
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
            Shown::Reg | Shown::Expand => Term::Reg(value),
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
}

/// What an instruction is called in a rule file, or nothing if the rules have no name for it.
///
/// The name carries the width, because a rule file that did not say how wide a term is would be
/// a file whose reader has to look at the line above to find out. Which widths there are names
/// for is the rule language's business and not this crate's: an instruction at a width nothing
/// is written about has no name here, and the answer to it is that no rule matches.
fn head_of(func: &Func, inst: Inst) -> Option<&'static str> {
    let data = &func[inst];
    let result = data.first_result?;
    let ty = func[result].ty;
    match data.opcode {
        Opcode::IConst => iconst_head(ty),
        Opcode::ICmp => {
            let Extra::IntPred(pred) = data.extra else { return None };
            Some(icmp_head(pred))
        }
        Opcode::SExt | Opcode::ZExt | Opcode::Trunc => {
            let from = func[*func[data.args].first()?].ty;
            convert_head(data.opcode, from, ty)
        }
        opcode => binary_head(opcode, ty),
    }
}

/// Which of the four widths a type is, or nothing for a width no rule is written at.
fn slot(ty: Type) -> Option<usize> {
    match ty.is_int().then(|| ty.bits())? {
        8 => Some(0),
        16 => Some(1),
        32 => Some(2),
        64 => Some(3),
        _ => None,
    }
}

/// What a value in a register is called at that width.
fn value_head(ty: Type) -> Option<&'static str> {
    Some(["value.i8", "value.i16", "value.i32", "value.i64"][slot(ty)?])
}

/// What a constant is called at that width.
fn iconst_head(ty: Type) -> Option<&'static str> {
    Some(["iconst.i8", "iconst.i16", "iconst.i32", "iconst.i64"][slot(ty)?])
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

/// What a conversion is called, which is the two widths it is between.
fn convert_head(opcode: Opcode, from: Type, to: Type) -> Option<&'static str> {
    let table: &[[Option<&'static str>; 4]; 4] = match opcode {
        Opcode::SExt => &SEXT,
        Opcode::ZExt => &ZEXT,
        Opcode::Trunc => &TRUNC,
        _ => return None,
    };
    table[slot(from)?][slot(to)?]
}

/// What each of the binary operations is called at each width.
fn binary_head(opcode: Opcode, ty: Type) -> Option<&'static str> {
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
    use rucc_ir::{Builder, Flags, Signature};

    use super::*;
    use crate::select::Subject;

    /// A function with one block, and the builder to put instructions in it.
    fn func() -> (Func, rucc_ir::Block) {
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
}
