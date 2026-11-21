//! A block parameter that the value ranges pin to one number becomes that number.
//!
//! [`crate::sccp`] makes a parameter a constant when every way into its block passes the same
//! constant. It does not read the tests above an edge, so it does not see a value that is known
//! only because of the way control came. The loop of `ffsv2_read` in NetBSD's libsa is the case
//! this pass was written for:
//!
//! ```c
//! while (size != 0) {
//!     ...
//!     rc = buf_read_file(f, &buf, &buf_size);
//!     if (rc)
//!         break;
//!     ...
//! }
//! ```
//!
//! The loop goes round again only where `rc` is zero, and `rc` is zero on the way in. So the value
//! of `rc` at the top of the loop is always zero. In the IR that value is a parameter of the loop
//! header, and the edge back passes the result of the call. Without this pass the parameter is a
//! value carried round the loop. On i386 at `-Os` it took a spill slot and a copy from one slot to
//! another on each turn, where gcc keeps nothing.
//!
//! # What is asked
//!
//! [`Ranges::of`] on each integer parameter of each reachable block except the entry. That is the
//! union over the ways in of what each argument can be at the end of the block that passes it,
//! and that range reads the tests above that block. The edge back in the loop above starts in a
//! block that only the zero arm of `if (rc)` reaches, so the result of the call is `[0, 0]` there.
//!
//! When the range is one number, the parameter gets a constant at the top of its block and its
//! readers read the constant. The parameter itself stays for the passes that remove unread ones, as
//! it does in [`crate::sccp`].
//!
//! # A factor
//!
//! An operand of a multiply that is one number where the multiply is, though not everywhere, is
//! asked about the same way with [`Ranges::at`], and becomes that number there. The kernel's
//! `accumulate_sum` adds `load * contrib` under `if (load)`, and `load` is `!!se->on_rq` once it is
//! inlined, so it is zero or one and the test says which. gcc multiplies by nothing there, and
//! `__update_load_avg_se` here kept two `imul`s and a register for each flag across the calls in
//! between. Once the operand is a one, [`crate::simplify`] takes the multiply away. Only an operand
//! that is two numbers or fewer is asked about, which a truth value widened to an integer is, so the
//! walk asks nothing about most of the multiplies it passes.
//!
//! # What is not here
//!
//! A pointer parameter. The ranges do not give a range for a pointer, and the null pointer that
//! every way in passes is already [`crate::sccp`]'s.
//!
//! An argument known only on its own edge, where the other ways in pass something else. The
//! parameter is not one number then, and an immediate in place of a register on one edge is not
//! smaller.

use rucc_base::hash::Map;
use rucc_ir::{Block, Extra, Func, Imm, Inst, InstData, Opcode, Value};

use crate::range::query::Ranges;
use crate::{Analyses, Analysis, Fuel, Pass, Phase, Preserved, Stats, uses};

/// The name the pipeline knows this pass by.
pub const NAME: &str = "pinned";

/// Recorded once for each parameter that became a constant.
const PINNED: &str = "parameter the value ranges pin to one number replaced by that number";

/// Recorded once for each parameter the ranges pin that the fuel did not reach.
const NO_FUEL: &str = "parameter the value ranges pin to one number kept, the pass ran out of fuel";

/// Recorded once for each operand of a multiply that became a constant.
const FACTOR: &str = "factor the value ranges pin to one number where it is read replaced by it";

/// Recorded once for each of those that the fuel did not reach.
const NO_FUEL_FACTOR: &str =
    "factor the value ranges pin to one number where it is read kept, the pass ran out of fuel";

/// The pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pinned;

impl Pass for Pinned {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "a block parameter the value ranges pin to one number becomes that number"
    }

    fn phase(&self) -> Phase {
        Phase::Scalar
    }

    fn preserves(&self) -> Preserved {
        // Constants at the top of blocks, and no edge touched. The parameters are read by fewer
        // things, which is the liveness, and a test on a constant is guessed differently, which is
        // the frequencies. This is what [`crate::sccp`] keeps for the same change.
        Preserved::ALL.without(Analysis::Liveness).without(Analysis::Frequencies)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let Some(entry) = func.entry() else { return stats };
        let (found, factors) = pinned(func, an, entry);
        for (inst, index, number) in factors {
            if !fuel.take() {
                stats.missed(NO_FUEL_FACTOR);
                continue;
            }
            let ty = func[func[func[inst].args][index]].ty;
            let at = func.add_imm(Imm::from_bits(number));
            let data = InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::IConst) };
            let span = func.span(inst);
            let made = func.create_inst(data, &[ty], span);
            func.insert_before(made, inst);
            let result = func[made].results().next().expect("a constant is one value");
            let mut args = func[func[inst].args].to_vec();
            args[index] = result;
            func[inst].args = func.push_values(&args);
            stats.optimized(FACTOR);
        }
        if found.is_empty() {
            return stats;
        }
        let mut forward = Map::default();
        for (block, param, number) in found {
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            let Some(first) = func.insts(block).next() else { continue };
            let ty = func[param].ty;
            let at = func.add_imm(Imm::from_bits(number));
            let data = InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::IConst) };
            let span = func.span(first);
            let made = func.create_inst(data, &[ty], span);
            func.insert_before(made, first);
            let result = func[made].results().next().expect("a constant is one value");
            forward.insert(param, result);
            stats.optimized(PINNED);
        }
        if !forward.is_empty() {
            uses::substitute(func, &forward);
        }
        stats
    }
}

/// The parameters [`pinned`] found with their blocks, and the factors with their multiplies and
/// which operand each one is, both with the number.
type Pins = (Vec<(Block, Value, u128)>, Vec<(Inst, usize, u128)>);

/// Each integer parameter the ranges pin to one number, and each operand of a multiply they pin
/// where it is read.
///
/// One [`Ranges`] for the whole walk, because the oracle keeps what it has worked out.
fn pinned(func: &Func, an: &Analyses, entry: Block) -> Pins {
    let cfg = an.cfg(func);
    let mut ranges = Ranges::cached(func, an);
    let mut found = Vec::new();
    let mut factors = Vec::new();
    for block in func.blocks() {
        if block == entry || !cfg.reaches(block) {
            continue;
        }
        for inst in func.insts(block) {
            if func[inst].opcode != Opcode::Mul {
                continue;
            }
            for (index, &operand) in func[func[inst].args].iter().enumerate() {
                let ty = func[operand].ty;
                if !ty.is_int() || !ty.is_scalar() {
                    continue;
                }
                let Some(could) = ranges.of(operand).list(2) else { continue };
                if could.len() < 2 {
                    continue;
                }
                if let Some(number) = ranges.at(operand, block).singleton() {
                    factors.push((inst, index, number));
                }
            }
        }
        for &param in &func[block].params {
            if !func[param].ty.is_int() {
                continue;
            }
            if let Some(number) = ranges.of(param).singleton() {
                found.push((block, param, number));
            }
        }
    }
    (found, factors)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rucc_base::Interner;

    use super::Pinned;
    use crate::outside::Outside;
    use crate::{Fuel, Pass};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// The module that text is, with the pass run over every function in it, printed, and checked
    /// by the verifier on the way out.
    fn run(body: &str, fuel: &mut Fuel) -> String {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let outside = Arc::new(Outside::of(&module));
        let ids: Vec<_> = module.funcs().collect();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses().about(Arc::clone(&outside));
            Pinned.run(&mut module[id], &mut an, fuel);
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    /// The loop of `ffsv2_read`. `%5` is zero on the way in, and the edge back passes the result of
    /// the call from the arm where that result is zero.
    const READ: &str = r#"
func @get() -> i32, linkage(external);

func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 0
    jump block1(%0, %1)
block1(%2: i32, %3: i32):
    %4 = icmp ne %2, %1
    br_if %4, block2, block4(%3)
block2:
    %5 = call @get() : () -> i32
    %6 = iconst.i32 0
    %7 = icmp ne %5, %6
    br_if %7, block4(%5), block3
block3:
    %8 = iconst.i32 1
    %9 = sub %2, %8
    jump block1(%9, %5)
block4(%10: i32):
    return %10
}
"#;

    /// The line that defines this value.
    fn def<'a>(out: &'a str, value: &str) -> &'a str {
        out.lines()
            .find(|line| line.trim().starts_with(&format!("{value} =")))
            .unwrap_or_else(|| panic!("{value} is defined, {out}"))
    }

    /// What the loop test passes to the exit when the loop is done.
    fn exit_argument(out: &str) -> String {
        let test = out
            .lines()
            .find(|line| line.trim().starts_with("br_if") && line.contains(", block2, block4("))
            .unwrap_or_else(|| panic!("the loop test is there, {out}"));
        let after = test.split("block4(").nth(1).expect("the exit has an argument");
        after.split(')').next().expect("the argument ends").to_string()
    }

    #[test]
    fn a_value_the_loop_goes_round_with_only_when_it_is_zero_is_zero() {
        let out = run(READ, &mut Fuel::unlimited());
        let passed = exit_argument(&out);
        assert!(def(&out, &passed).contains("iconst.i32 0"), "the exit passes a zero, {out}");
    }

    /// The same loop with the arms of the test the other way round, so the edge back is from the
    /// arm where the call gave something other than zero. Nothing is known about it there.
    #[test]
    fn a_value_the_loop_goes_round_with_when_it_is_not_zero_is_kept() {
        let swapped = READ.replace("br_if %7, block4(%5), block3", "br_if %7, block3, block4(%5)");
        let out = run(&swapped, &mut Fuel::unlimited());
        assert_eq!(exit_argument(&out), "%3", "{out}");
    }

    /// `accumulate_sum` with `load` inlined as `!!on_rq`: under `if (load)` the factor is one.
    const SUM: &str = r#"
func @f(i32, i64) -> i64, linkage(external) {
block0(%0: i32, %1: i64):
    %2 = iconst.i32 0
    %3 = icmp ne %0, %2
    %4 = zext.i64 %3
    %5 = iconst.i64 0
    %6 = icmp ne %4, %5
    br_if %6, block1, block2
block1:
    %7 = mul %4, %1
    return %7
block2:
    return %5
}
"#;

    #[test]
    fn a_factor_a_test_settles_is_that_number() {
        let out = run(SUM, &mut Fuel::unlimited());
        let product = out.lines().find(|line| line.contains("= mul "));
        let product = product.unwrap_or_else(|| panic!("the multiply is there, {out}"));
        let factor = product.split("mul ").nth(1).and_then(|args| args.split(',').next());
        let factor = factor.unwrap_or_else(|| panic!("the multiply has two operands, {out}"));
        assert!(def(&out, factor).contains("iconst.i64 1"), "the factor is a one, {out}");
    }

    #[test]
    fn with_no_fuel_nothing_changes() {
        let out = run(READ, &mut Fuel::of(0));
        assert_eq!(exit_argument(&out), "%3", "{out}");
    }
}
