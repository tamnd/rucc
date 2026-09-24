//! What each AArch64 instruction costs, and which part of the machine it costs it on.
//!
//! Design: `spec/optimizer/38-scheduling-and-layout.md` sections 38.1 and 38.6. The shape is
//! [`crate::TimingInsts`] and the arrangement is the one `rucc_target::x86_64::TIMING` has: a
//! number per form, and the handful of instructions that are slower than their form written out
//! one at a time in [`slow`].
//!
//! # Where the numbers come from
//!
//! Arm's software optimization guide for the Neoverse N1, which is the core [`MODEL`] names. N1
//! rather than something newer because it is the core in most of the AArch64 servers a program is
//! run on today, because the Cortex-A76 in a phone is the same pipeline, and because the cores
//! after it moved the divides and the conversions by a cycle or two and nothing else here. Apple's
//! cores are wider and faster at nearly everything, and the scheduler makes much the same decisions
//! for them from these numbers, since what it acts on is which instructions are slow rather than by
//! how much.
//!
//! The numbers are the register forms. A load is [`LOAD`], which is the first level cache hit,
//! for the reason the x86 model gives: a scheduler cannot know which loads miss.
//!
//! # An opcode that is two instructions
//!
//! A comparison that keeps its answer is a `cmp` and a `cset`, and the address of a symbol is an
//! `adrp` and an `add`. The number for one of those is the two one after the other, since the
//! second reads what the first wrote, which is what the x86 model does for its comparison and byte.

use crate::aarch64::insts::{Form, form};
use crate::timing::{Timing, TimingInsts, Unit};

/// Which processor the numbers describe.
pub const MODEL: &str = "Arm Neoverse N1, from Arm's published software optimization guide";

/// How many cycles after a load starts before what it read may be used.
pub const LOAD: u32 = 4;

/// How many instructions this machine starts in one cycle.
const WIDTH: u32 = 4;

/// The AArch64 timing model.
pub static TIMING: TimingInsts = TimingInsts {
    prefix: "a64.",
    model: MODEL,
    // Not an automaton either, for the reason the x86 model is not one.
    accurate: false,
    width: WIDTH,
    slots,
    timing,
};

/// How many of each unit this machine has.
///
/// Three integer pipelines, one of which also multiplies and divides, which is why the multiplier
/// and the divider are one each. Two pipelines that load and one that stores, as the guide counts
/// them, one branch unit, and two floating point pipelines with the divider on one of them.
fn slots(unit: Unit) -> u32 {
    match unit {
        Unit::Int => 3,
        Unit::Mul => 1,
        Unit::Div => 1,
        Unit::Load => 2,
        Unit::Store => 1,
        Unit::Branch => 1,
        Unit::Float => 2,
        Unit::FloatDiv => 1,
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
        // A constant, a piece of one, the arithmetic, a copy, a widening, an address computed from
        // a base, a comparison and a choice on one. One cycle each on any of the three pipelines.
        LoadImm | Insert | Alu | AluI | Unary | Convert | Move | Lea | Cmp | CmpI | Test | Csel
        | Set => (1, Unit::Int),
        // A comparison and the `cset` or `csel` behind it, which waits for the comparison.
        CmpSet | CmpSetI | Select => (2, Unit::Int),
        // The page of a symbol and the offset into it, one after the other.
        Address => (2, Unit::Int),
        // A multiply and add, on the one pipeline that multiplies.
        MulAdd => (2, Unit::Mul),
        Load => (LOAD, Unit::Load),
        Store | Probe => (1, Unit::Store),
        Push | PushPair => (1, Unit::Store),
        Pop | PopPair => (LOAD, Unit::Load),
        LoadFp => (LOAD + 1, Unit::Load),
        StoreFp => (1, Unit::Store),
        // Floating point. An addition is two cycles and a multiply three on this core, and the
        // multiply is overridden in `slow` so this is the addition's number. The divides and the
        // square roots are there as well.
        FAlu => (2, Unit::Float),
        FUnary | FConvert => (2, Unit::Float),
        FMove => (2, Unit::Float),
        // A general register into the other file and back, as a number or as bits. Both go across
        // the machine and the conversion rounds on the way, which the guide prices the same.
        IntToFp => (3, Unit::Float),
        FpToInt => (3, Unit::Float),
        FCmp => (2, Unit::Float),
        // The comparison, then the `cset` that reads what it left, which is back on the integer
        // side and a cycle more.
        FCmpSet => (3, Unit::Float),
        Jump | Jcc | JumpAway => (1, Unit::Branch),
        JumpReg | Call | Ret => (2, Unit::Branch),
        RetVal | RetVal2 | RetValFp | RetVal2Fp | RetVal3Fp | RetVal4Fp | ArgVal | ArgValFp
        | BrCond => (0, Unit::Free),
        Barrier | Trap | Nop => (1, Unit::Fixed),
    };
    Timing { latency, unit }
}

/// The instructions whose cost is not the cost of their form.
fn slow(name: &str) -> Option<Timing> {
    let stem = name.split('_').next().unwrap_or(name);
    let wide = name.ends_with("_64") || name.ends_with("_f64");
    let (latency, unit) = match stem {
        // A multiply, on the one pipeline that does it. The two that keep the high half of a
        // hundred and twenty eight bit product are a cycle slower.
        "mul" => (2, Unit::Mul),
        "smulh" | "umulh" => (3, Unit::Mul),
        // A divide, whose cost is how many bits it divides. The guide gives a range for each, and
        // the number here is the top of it, since a divide is slow either way and the top is the
        // side it is safe to be wrong on.
        "sdiv" | "udiv" => (if wide { 20 } else { 12 }, Unit::Div),
        "fmul" => (3, Unit::Float),
        "fdiv" => (if wide { 15 } else { 10 }, Unit::FloatDiv),
        "fsqrt" => (if wide { 17 } else { 10 }, Unit::FloatDiv),
        // The address of a symbol through the global offset table is a page and a load, and the
        // offset of one from the thread pointer is the pointer and two additions.
        "got" => (1 + LOAD, Unit::Load),
        "tls" => (3, Unit::Int),
        // The copy of the stack pointer and the mask, one behind the other.
        "align" => (2, Unit::Int),
        _ => return None,
    };
    Some(Timing { latency, unit })
}

#[cfg(test)]
mod tests {
    use super::{LOAD, TIMING};
    use crate::aarch64::insts::INSTS;
    use crate::timing::Unit;

    #[test]
    fn every_instruction_this_target_has_is_one_the_model_has_a_number_for() {
        for &(name, _) in INSTS {
            assert!(TIMING.of(name).is_some(), "{name} has no timing");
        }
        assert_eq!(TIMING.of("a64.fmla_rrr_f64"), None);
        assert_eq!(TIMING.of("a64.add_rr_32"), TIMING.of("add_rr_32"));
    }

    #[test]
    fn the_slow_instructions_are_slower_than_an_addition_and_on_a_unit_there_is_one_of() {
        let add = TIMING.of("a64.add_rr_64").expect("described");
        for name in ["a64.mul_rr_64", "a64.umulh_rr_64", "a64.madd_rrr_32"] {
            let timing = TIMING.of(name).expect("described");
            assert!(timing.latency > add.latency, "{name}");
            assert_eq!(timing.unit, Unit::Mul, "{name}");
        }
        let narrow = TIMING.of("a64.sdiv_rr_32").expect("described");
        let wide = TIMING.of("a64.udiv_rr_64").expect("described");
        assert!(wide.latency > narrow.latency);
        assert_eq!(wide.unit, Unit::Div);
        assert_eq!(TIMING.slots(Unit::Div), 1);
        assert_eq!(TIMING.of("a64.fdiv_f64").expect("described").unit, Unit::FloatDiv);
        assert_eq!(TIMING.of("a64.fadd_f64").expect("described").unit, Unit::Float);
    }

    #[test]
    fn a_load_costs_what_the_cache_takes_and_an_address_through_the_table_costs_one_as_well() {
        assert_eq!(TIMING.of("a64.ldr_64").expect("described").latency, LOAD);
        assert_eq!(TIMING.of("a64.got_64").expect("described").latency, 1 + LOAD);
        assert_eq!(TIMING.of("a64.got_64").expect("described").unit, Unit::Load);
    }

    #[test]
    fn an_instruction_that_encodes_to_nothing_takes_no_time_and_one_that_must_not_move_is_fixed() {
        for name in ["a64.arg_val_32", "a64.ret_val_f64", "a64.br_cond_32"] {
            assert_eq!(TIMING.of(name).expect("described").unit, Unit::Free, "{name}");
            assert_eq!(TIMING.of(name).expect("described").latency, 0, "{name}");
        }
        for name in ["a64.fence", "a64.trap", "a64.nop"] {
            assert_eq!(TIMING.of(name).expect("described").unit, Unit::Fixed, "{name}");
        }
        assert!(!TIMING.accurate);
        for &unit in Unit::ALL {
            assert!(TIMING.slots(unit) >= 1, "{unit:?}");
        }
    }
}
