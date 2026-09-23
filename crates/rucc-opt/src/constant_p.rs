//! What `__builtin_constant_p` answers about a value the front end could not see to be a constant.
//!
//! Design: `spec/optimizer/20-idioms-and-libcalls.md`, and tamnd/rucc#392.
//!
//! ```c
//! int size = sizeof (int);
//! if (__builtin_constant_p (size))   /* one at -O2, zero at -O0 */
//! ```
//!
//! gcc answers the builtin once it has optimized the function, which is why the same line gives
//! zero at `-O0` and one at `-O2`: by the time the question is asked, `size` has become the four it
//! was set to. The front end answers what it can see, a constant as written is one and an argument
//! with an effect is zero, and leaves the rest as an `is_constant` instruction for this pass. It
//! answers one where the operand has become a constant and zero everywhere else, and it runs late
//! enough in each level that the folding in front of it has had its chance, and early enough that
//! the folding and the control flow pass behind it take out the arm the answer did not choose.
//!
//! At `-O0` every question is answered zero before any other pass runs, by [`answer`], since that
//! is gcc's answer at that level and the lowering already turns some locals into constants that gcc
//! would still be reading out of memory. The same function answers whatever is left once the passes
//! have run, where the pass list did not have this one in it, because nothing below the optimizer
//! lowers the instruction.

use rucc_ir::{Block, Def, Extra, Func, FuncId, Imm, Inst, InstData, Module, Opcode, Value};

use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// What the pass calls itself, which is what `-fdump-ir=after-constant-p` spells.
pub const NAME: &str = "constant-p";

const ANSWERED: &str = "__builtin_constant_p answered";

#[derive(Debug)]
pub struct ConstantP;

impl Pass for ConstantP {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "__builtin_constant_p is one where the value has become a constant and zero elsewhere"
    }

    fn preserves(&self) -> Preserved {
        // A constant where an instruction was, in the same block, and no edge touched.
        Preserved::ALL
    }

    fn required(&self) -> bool {
        // Nothing below the optimizer lowers the instruction, so a run without this pass is a
        // compile that stops on a construct the program never wrote.
        true
    }

    fn run(&self, func: &mut Func, _an: &mut Analyses, _fuel: &mut Fuel) -> Stats {
        // No fuel is asked for, because leaving a question unanswered is not a smaller rewrite but
        // an instruction the back end refuses. The same reason `expect` gives.
        let mut stats = Stats::new();
        for _ in 0..settle(func, true) {
            stats.optimized(ANSWERED);
        }
        stats
    }
}

/// Answers every `is_constant` in the module and says how many there were.
///
/// `look` is false at `-O0`, where every question is answered zero.
pub fn answer(module: &mut Module, look: bool) -> usize {
    let mut answered = 0;
    for id in module.funcs().collect::<Vec<FuncId>>() {
        if !module[id].is_declaration() {
            answered += settle(&mut module[id], look);
        }
    }
    answered
}

/// Answers every `is_constant` in the function and says how many there were.
fn settle(func: &mut Func, look: bool) -> usize {
    let asked: Vec<Inst> = func
        .blocks()
        .collect::<Vec<Block>>()
        .into_iter()
        .flat_map(|block| func.insts(block).collect::<Vec<Inst>>())
        .filter(|&inst| func[inst].opcode == Opcode::IsConstant)
        .collect();
    for &inst in &asked {
        let known =
            look && func[func[inst].args].first().is_some_and(|&value| is_constant(func, value));
        write(func, inst, i128::from(known));
    }
    asked.len()
}

/// Whether this value is a constant, an integer or a floating point one.
fn is_constant(func: &Func, value: Value) -> bool {
    let Def::Result { inst, .. } = func[value].def else { return false };
    matches!(func[inst].opcode, Opcode::IConst | Opcode::FConst)
}

/// Puts the answer where the question was.
fn write(func: &mut Func, inst: Inst, number: i128) {
    let result = func[inst].results().next().expect("an answer is one value");
    let ty = func[result].ty;
    let span = func.span(inst);
    let imm = func.add_imm(Imm::int(number, ty.lane()));
    let data = InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
    let made = func.create_inst(data, &[ty], span);
    func.insert_before(made, inst);
    let value = func[made].results().next().expect("a constant is one value");
    let forward = [(result, value)].into_iter().collect();
    crate::uses::substitute(func, &forward);
    func.remove_inst(inst);
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;

    const TEXT: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"

func @use(i32, i32, i32), linkage(external);

func @g(i32), linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 4
    %2 = is_constant.i32 %1
    %3 = is_constant.i32 %0
    %4 = fconst.f64 0x3ff0000000000000
    %5 = is_constant.i32 %4
    call @use(%2, %3, %5) : (i32, i32, i32)
    return
}
"#;

    fn answered(look: bool) -> String {
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(TEXT, &mut names).expect("the fixture parses");
        assert_eq!(answer(&mut module, look), 3);
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the answers left invalid IR, {errors:?}");
        }
        rucc_ir::print(&module, &names)
    }

    /// A constant is one and a parameter is zero, whether the constant is an integer or not.
    #[test]
    fn a_value_that_became_a_constant_is_one_and_anything_else_is_zero() {
        let out = answered(true);
        assert!(!out.contains("is_constant"), "{out}");
        assert_eq!(out.matches("iconst.i32 1").count(), 2, "{out}");
        assert_eq!(out.matches("iconst.i32 0").count(), 1, "{out}");
    }

    /// `-O0` answers zero to all of them, the constants included, which is what gcc answers there.
    #[test]
    fn nothing_is_a_constant_at_o0() {
        let out = answered(false);
        assert!(!out.contains("is_constant"), "{out}");
        assert!(!out.contains("iconst.i32 1"), "{out}");
        assert_eq!(out.matches("iconst.i32 0").count(), 3, "{out}");
    }
}
