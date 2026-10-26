//! What each RISC-V instruction costs, and which part of the machine it costs it on.
//!
//! Design: `spec/optimizer/38-scheduling-and-layout.md` sections 38.1 and 38.6. The shape is
//! [`crate::TimingInsts`] and the arrangement is the one `rucc_target::aarch64::TIMING` has: a
//! number per form, and the instructions that are slower than their form in [`slow`].
//!
//! # Where the numbers come from
//!
//! SiFive's manual for the U74, which is the core [`MODEL`] names. It is the core in the boards a
//! RISC-V Linux kernel is most often run on, and it is in order and two wide, which is the kind of
//! machine where the order the scheduler picks matters most. The numbers are the register forms,
//! and a load is [`LOAD`], the first level cache hit.
//!
//! An opcode that is two instructions, such as a comparison that keeps its answer, costs the two
//! one after the other, since the second reads what the first wrote.

use crate::riscv64::insts::{Form, form};
use crate::timing::{Timing, TimingInsts, Unit};

/// Which processor the numbers describe.
pub const MODEL: &str = "SiFive U74, from SiFive's published core manual";

/// How many cycles after a load starts before what it read may be used.
pub const LOAD: u32 = 3;

/// How many instructions this machine starts in one cycle.
const WIDTH: u32 = 2;

/// The RISC-V timing model.
pub static TIMING: TimingInsts = TimingInsts {
    prefix: "rv.",
    model: MODEL,
    // Not an automaton, for the reason the x86 model is not one.
    accurate: false,
    width: WIDTH,
    slots,
    timing,
};

/// How many of each unit this machine has.
///
/// Two integer pipelines, one of which also multiplies, divides, loads, stores and branches, which
/// is why every one of those is one. One floating point pipeline with the divider in it.
fn slots(unit: Unit) -> u32 {
    match unit {
        Unit::Int => 2,
        Unit::Mul
        | Unit::Div
        | Unit::Load
        | Unit::Store
        | Unit::Branch
        | Unit::Float
        | Unit::FloatDiv => 1,
        Unit::Free | Unit::Fixed => WIDTH,
    }
}

/// What an instruction of that name costs.
fn timing(name: &str) -> Option<Timing> {
    let form = form(name)?;
    Some(slow(name).unwrap_or_else(|| plain(form)))
}

/// The cost of every instruction whose cost is the cost of its form.
fn plain(form: Form) -> Timing {
    use Form::*;
    let (latency, unit) = match form {
        // A constant, the arithmetic, a copy, a widening and an address from a base. A constant
        // that takes more than one instruction is rare enough not to price apart.
        LoadImm | Alu | AluI | Unary | Convert | Move | Lea | Test => (1, Unit::Int),
        // A comparison and the instruction behind it that reads it, and a choice, which is a
        // copy, a branch and a copy.
        CmpSet | Select => (2, Unit::Int),
        // The `auipc` and the `addi` behind it.
        Address => (2, Unit::Int),
        Load => (LOAD, Unit::Load),
        Store | Probe | Push | PushPair => (1, Unit::Store),
        Pop | PopPair => (LOAD, Unit::Load),
        LoadFp => (LOAD, Unit::Load),
        StoreFp => (1, Unit::Store),
        FAlu => (4, Unit::Float),
        FMulAdd => (5, Unit::Float),
        FUnary | FMove => (2, Unit::Float),
        FConvert | IntToFp | FpToInt => (4, Unit::Float),
        FCmpSet => (4, Unit::Float),
        Jump | Jcc | JumpAway | JumpZero => (1, Unit::Branch),
        JumpReg | Call | Ret => (2, Unit::Branch),
        RetVal | RetVal2 | RetValFp | RetVal2Fp | ArgVal | ArgValFp | BrCond => (0, Unit::Free),
        Barrier | Trap | Nop | Template => (1, Unit::Fixed),
    };
    Timing { latency, unit }
}

/// The instructions whose cost is not the cost of their form.
fn slow(name: &str) -> Option<Timing> {
    let stem = name.split('_').next().unwrap_or(name);
    let wide = name.ends_with("_64") || name.ends_with("_f64");
    let (latency, unit) = match stem {
        "mul" | "mulh" | "mulhu" => (3, Unit::Mul),
        // A divide takes as long as the bits it divides, and the number is the top of the range
        // the manual gives, which is the side it is safe to be wrong on.
        "div" | "divu" | "rem" | "remu" => (if wide { 34 } else { 20 }, Unit::Div),
        "fdiv" => (if wide { 33 } else { 20 }, Unit::FloatDiv),
        "fsqrt" => (if wide { 40 } else { 25 }, Unit::FloatDiv),
        // The address through the global offset table, which is an `auipc` and a load.
        "got" | "gottprel" => (1 + LOAD, Unit::Load),
        _ => return None,
    };
    Some(Timing { latency, unit })
}

#[cfg(test)]
mod tests {
    use super::{LOAD, TIMING};
    use crate::riscv64::insts::INSTS;
    use crate::timing::Unit;

    #[test]
    fn every_instruction_this_target_has_is_one_the_model_has_a_number_for() {
        for &(name, _) in INSTS {
            assert!(TIMING.of(name).is_some(), "{name} has no timing");
        }
        assert_eq!(TIMING.of("rv.madd_rrr_64"), None);
        assert_eq!(TIMING.of("rv.add_rr_32"), TIMING.of("add_rr_32"));
    }

    #[test]
    fn the_slow_instructions_are_slower_than_an_addition_and_on_a_unit_there_is_one_of() {
        let add = TIMING.of("rv.add_rr_64").expect("described");
        for name in ["rv.mul_rr_32", "rv.mulhu_rr_64"] {
            let timing = TIMING.of(name).expect("described");
            assert!(timing.latency > add.latency, "{name}");
            assert_eq!(timing.unit, Unit::Mul, "{name}");
        }
        let narrow = TIMING.of("rv.div_rr_32").expect("described");
        let wide = TIMING.of("rv.remu_rr_64").expect("described");
        assert!(wide.latency > narrow.latency);
        assert_eq!(wide.unit, Unit::Div);
        assert_eq!(TIMING.slots(Unit::Div), 1);
        assert_eq!(TIMING.of("rv.fdiv_f64").expect("described").unit, Unit::FloatDiv);
        assert_eq!(TIMING.of("rv.fadd_f64").expect("described").unit, Unit::Float);
    }

    #[test]
    fn a_load_costs_what_the_cache_takes_and_an_address_through_the_table_costs_one_as_well() {
        assert_eq!(TIMING.of("rv.ld_64").expect("described").latency, LOAD);
        assert_eq!(TIMING.of("rv.got_64").expect("described").latency, 1 + LOAD);
        assert_eq!(TIMING.of("rv.got_64").expect("described").unit, Unit::Load);
    }

    #[test]
    fn an_instruction_that_encodes_to_nothing_takes_no_time_and_one_that_must_not_move_is_fixed() {
        for name in ["rv.arg_val_32", "rv.ret_val_f64", "rv.br_cond_32"] {
            assert_eq!(TIMING.of(name).expect("described").unit, Unit::Free, "{name}");
            assert_eq!(TIMING.of(name).expect("described").latency, 0, "{name}");
        }
        for name in ["rv.fence", "rv.trap", "rv.nop"] {
            assert_eq!(TIMING.of(name).expect("described").unit, Unit::Fixed, "{name}");
        }
        assert!(!TIMING.accurate);
        for &unit in Unit::ALL {
            assert!(TIMING.slots(unit) >= 1, "{unit:?}");
        }
    }
}
