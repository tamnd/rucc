//! What each x86-64 instruction costs, and which part of the machine it costs it on.
//!
//! Design: `spec/optimizer/38-scheduling-and-layout.md` sections 38.1 and 38.6. The shape of the
//! description is [`crate::TimingInsts`], and the module comment there says why a model is allowed
//! to be approximate and what it has to do about being approximate.
//!
//! # Where the numbers come from
//!
//! The published latency and reciprocal throughput tables for Intel's Skylake core, which is the
//! microarchitecture the numbers here describe and the one [`MODEL`] names. Skylake rather than
//! something newer because it is the oldest core still common enough to be worth compiling for,
//! because every core since is within a cycle of it on nearly every instruction here, and because
//! the instructions it is not within a cycle of are the divides, which are slow enough on all of
//! them that the scheduler makes the same decision either way.
//!
//! The numbers are the register to register case. An instruction that reads memory is that number
//! plus [`LOAD`], which is the load to use latency of the first level cache and is the one number
//! here that a cache miss makes wrong by two orders of magnitude. A scheduler cannot know which
//! loads miss, so it schedules for the case it can know about, which is what every scheduler does.
//!
//! # Why it is a function of the form and not a table of four hundred rows
//!
//! Because the answer is a function of the form for nearly every instruction: every addition of
//! every width costs the same and so does every comparison, so a row per name would be four
//! hundred rows saying nine things. What is not a function of the form is the handful of
//! instructions that are slow, and those are written out one at a time in [`slow`] where they can
//! be read and argued with.
//!
//! # What it does not say
//!
//! That this machine executes out of order. It does, it has a window of a couple of hundred
//! instructions, and it will find much of the parallelism a scheduler finds without being asked.
//! That is exactly why section 38.8 owes a measurement of scheduling on and off on this target and
//! why spec 10.5 expects the answer to be near zero here. The model is honest about the machine's
//! timings and says nothing about how much reordering is worth, because how much it is worth is a
//! measurement rather than a description.

use crate::timing::{Timing, TimingInsts, Unit};
use crate::x86_64::insts::{Form, form};

/// Which processor the numbers describe.
pub const MODEL: &str =
    "Intel Skylake, from the published instruction latency and throughput tables";

/// How many cycles after a load starts before what it read may be used.
///
/// The first level cache hit case. Everything here that touches memory is some other instruction's
/// cost plus this one.
pub const LOAD: u32 = 5;

/// How many instructions this machine starts in one cycle.
const WIDTH: u32 = 4;

/// The x86-64 timing model.
pub static TIMING: TimingInsts = TimingInsts {
    prefix: "x64.",
    model: MODEL,
    // It is not. There is no automaton here, the unit counts are a summary of a port layout rather
    // than the port layout, and nothing has measured what this machine does when a queue fills.
    // Saying so is what keeps `rucc_codegen::schedule` from holding an instruction back over a
    // number nobody checked. See the module comment in `crate::timing`.
    accurate: false,
    width: WIDTH,
    slots,
    timing,
};

/// How many of each unit this machine has.
///
/// Four integer units, which is the width, so integer work is limited by the width and not by the
/// units. One multiplier and one divider, which is what makes a block of multiplies slower than a
/// block of additions however independent they are. Two address units and one store unit, which is
/// the shape every core of this family has had. Two floating point units and one floating point
/// divider, for the same reason the integer side has one of each.
fn slots(unit: Unit) -> u32 {
    match unit {
        Unit::Int => 4,
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
    Some(slow(name, form).unwrap_or_else(|| plain(form)))
}

/// The cost of every instruction whose cost is the cost of its form, which is most of them.
fn plain(form: Form) -> Timing {
    use Form::*;
    let (latency, unit) = match form {
        // A register write of a constant, and every ordinary arithmetic instruction. One cycle on
        // every core of this family, and there are as many units for them as the machine is wide.
        // The conditional move on its own belongs here rather than with the pair below it: what it
        // waits for is a comparison somebody else wrote, and once those bits are there it is the
        // one cycle the pair's second half is.
        LoadImm | AluRr | AluRi | UnaryR | ShiftRi | Move | Lea | Convert | Cmp | CmpRi | Test
        | Set | Cmov => (1, Unit::Int),
        // A shift by a count in a register, which is the one shift that is not one cycle. The
        // machine has to read the count register and the condition state together, and what it
        // does about that has cost a cycle on every core here.
        ShiftCl => (2, Unit::Int),
        // A search for a set bit and the counts spelled the same way, which are the slowest
        // instructions on this side that are not divides. Three cycles is what every core in this
        // family has published for all four of them, and it is three rather than one because what
        // they answer is a position rather than an operation on each bit in place.
        Search => (3, Unit::Int),
        // Turning a register round, which is one cycle on every core in this family. It reads and
        // writes the whole register and there is no carry between any two bytes of it, so the
        // machine does it in the same cycle an addition takes.
        Swap => (1, Unit::Int),
        // A comparison and the byte behind it, which is two instructions written as one name. The
        // byte cannot start until the comparison has set the bits it reads, so the pair costs the
        // two of them one after the other rather than the slower of the two.
        CmpSet | CmpSetRi | TestCmov => (2, Unit::Int),
        // The same arithmetic reading its second source out of memory. The load is the whole of
        // the extra cost and the unit it needs is the address unit, because that is what is scarce
        // about it: the arithmetic behind the load is one cycle on a unit there are four of.
        AluRm => (1 + LOAD, Unit::Load),
        // The same arithmetic with the answer left in memory, which is a load and the arithmetic
        // and a store. The store is the scarce part, the way the load is the scarce part above,
        // and the latency is not a number anything waits on for the reason a store's is not:
        // nothing in a register is waiting for it.
        AluMr => (1 + LOAD, Unit::Store),
        // The same again with the other source a constant, which costs what the register form
        // costs. Where the second source came from is not something the machine spends a cycle on
        // once the address is known, and a constant is on the instruction.
        AluMi => (1 + LOAD, Unit::Store),
        // A comparison reading its right hand side out of memory, with and without the byte
        // behind it. Each is its register form plus a load, and the unit is the address unit for
        // the reason it is above: the comparison itself is a cycle on a unit there are four of and
        // the load is the part there is not.
        CmpRm => (1 + LOAD, Unit::Load),
        CmpSetRm => (2 + LOAD, Unit::Load),
        // A division. Overridden by width in `slow`, and this is what is left for a form that
        // reaches here without one, which nothing does.
        DivQuo | DivRem => (26, Unit::Div),
        // A load, and a store. A store's latency is not a number anything reads, since nothing in
        // a register waits on it, but it does take the one store unit while it runs.
        Load => (LOAD, Unit::Load),
        Store => (1, Unit::Store),
        // The stack. A push is a store, a pop is a load, and neither is any cheaper than the
        // ordinary one because the address it uses is an ordinary address.
        Push => (1, Unit::Store),
        Pop => (LOAD, Unit::Load),
        // Control flow. A jump the machine predicts costs about nothing and a jump it does not
        // costs about fifteen, and which one it is is not something a schedule changes, so the
        // number here is the one that matters for ordering: it takes the one branch unit.
        Jcc | Jmp => (1, Unit::Branch),
        JmpReg | Call | Ret => (2, Unit::Branch),
        // A read or write of a memory location the whole machine agrees about. The locked forms
        // are tens of cycles and the number here is a floor on that rather than a measurement,
        // because what a scheduler needs to know about them is that they are expensive and that
        // nothing reorders around them, and the second is not this table's answer.
        CmpXchg | Rmw => (20, Unit::Store),
        // Floating point, register to register. Four cycles for an addition or a multiply is the
        // number this family has had since it stopped having separate adders and multipliers.
        // Divides are overridden in `slow`.
        AluVec => (4, Unit::Float),
        MoveVec => (1, Unit::Float),
        LoadVec => (1 + LOAD, Unit::Load),
        StoreVec => (1, Unit::Store),
        // The conversions, which are the slowest instructions on this side that are not divides:
        // each is more than one operation inside the machine and the published numbers say so.
        ConvertVec | ConvertToVec => (5, Unit::Float),
        ConvertFromVec => (4, Unit::Float),
        // A floating point comparison and the byte behind it, which is the floating point side of
        // `CmpSet` and costs the same way: the comparison, then the byte that reads its bits.
        CmpSetVec => (3, Unit::Float),
        CmpSetVecBoth => (4, Unit::Float),
        // The old stack based floating point unit, which this target reaches for `long double` and
        // for nothing else. Every one of these goes through memory, and the numbers are the ones
        // this family has kept for compatibility rather than improved.
        PushX87 => (LOAD, Unit::Load),
        PopX87 => (4, Unit::Store),
        CtrlX87 => (8, Unit::Fixed),
        ArithX87 => (5, Unit::Float),
        UnaryX87 => (1, Unit::Float),
        CmpSetX87 => (4, Unit::Float),
        CmpSetX87Both => (5, Unit::Float),
        // The instructions that encode to nothing. They are in the function to tell the allocator
        // where a value already is, and a schedule built around them taking a cycle would be a
        // schedule built around instructions that are not in the output.
        RetVal | RetVal2 | ArgVal | BrCond | RetValVec | RetVal2Vec | ArgValVec => (0, Unit::Free),
        // What is left: a fence, a trap, a landing pad, a pad, a spin hint and an alignment. None
        // of them produces a value anything waits on, and every one of them is in the function for
        // a reason its operands do not say, which is what `Unit::Fixed` is and what stops a
        // schedule from moving one or from moving anything past one. The alignment needs the second
        // half of that more than anything else here does: what it is about is which instruction
        // comes after it, so an instruction moved across one is an alignment of something other
        // than what the program pointed at.
        Barrier | Trap | Landing | Nop | Spin | Align => (1, Unit::Fixed),
        Prefetch => (1, Unit::Load),
        // Asking the processor about itself, which drains it first. It is the most expensive
        // instruction in this table by a long way, in the hundreds of cycles on every machine
        // anyone has measured, and the number here is a floor on that rather than a measurement
        // for the same reason the locked forms carry one: what a schedule needs to know is that
        // it is expensive and that nothing moves past it, and `Unit::Fixed` is the second.
        CpuId => (100, Unit::Fixed),
    };
    Timing { latency, unit }
}

/// The instructions whose cost is not the cost of their form.
///
/// Every one of them is slow, and every one of them is slow for a reason that is about the
/// operation rather than about the shape of the instruction: a multiply is not an addition and a
/// divide is not a multiply, however alike the three look in an operand vector.
fn slow(name: &str, form: Form) -> Option<Timing> {
    let stem = name.split('_').next().unwrap_or(name);
    let float = matches!(form, Form::AluVec | Form::ArithX87);
    match stem {
        // An integer multiply, on the one multiplier. Three cycles at every width this target
        // writes one at, including the eight bit one, which is written as a thirty two bit
        // multiply and so costs what that costs.
        "imul" => Some(Timing {
            latency: if form == Form::AluRm { 3 + LOAD } else { 3 },
            unit: Unit::Mul,
        }),
        // A divide, which is the only instruction here whose cost depends on how many bits it is
        // dividing. The narrow ones are one pass through the divider and the sixty four bit one is
        // several, which is why it is most of twice the cost rather than a little more.
        "div" | "idiv" if !float => Some(Timing { latency: width(name), unit: Unit::Div }),
        // A floating point divide, on the one floating point divider. The double precision one is
        // slower than the single precision one for the reason the wide integer divide is slower
        // than the narrow one, and by about the same proportion.
        "divss" => Some(Timing { latency: 11, unit: Unit::FloatDiv }),
        "divsd" => Some(Timing { latency: 14, unit: Unit::FloatDiv }),
        "fdiv" | "fdivr" => Some(Timing { latency: 15, unit: Unit::FloatDiv }),
        // The old unit's addition, which is faster than its multiply, which is the other way round
        // from the current one where the two are the same instruction underneath.
        "fadd" | "fsub" | "fsubr" => Some(Timing { latency: 3, unit: Unit::Float }),
        // A move of the bits of a register between the two files, which shares its name with the
        // conversions and is not one: nothing is rounded, nothing is checked, and it costs what a
        // move across the machine costs rather than what a conversion costs.
        "movd" | "movq" => Some(Timing { latency: 2, unit: Unit::Float }),
        _ => None,
    }
}

/// How long a divide of the width in that name takes.
///
/// The width is the last part of the name, which every arithmetic instruction on this target
/// carries. A name without one is not a divide this target has, and the answer for it is the wide
/// case, which is the slow one and so the one that is safe to be wrong in the direction of.
fn width(name: &str) -> u32 {
    match name.rsplit('_').next() {
        Some("8") | Some("16") => 25,
        Some("32") => 26,
        _ => 42,
    }
}

#[cfg(test)]
mod tests {
    use super::{LOAD, TIMING};
    use crate::timing::Unit;
    use crate::x86_64::insts::INSTS;

    #[test]
    fn every_instruction_this_target_has_is_one_the_model_has_a_number_for() {
        for &(name, _) in INSTS {
            assert!(TIMING.of(name).is_some(), "{name} has no timing");
        }
    }

    #[test]
    fn a_name_this_target_does_not_have_gets_no_number_rather_than_a_made_up_one() {
        assert_eq!(TIMING.of("x64.fma_rrr_64"), None);
        assert_eq!(TIMING.of("not_even_prefixed"), None);
    }

    #[test]
    fn the_prefix_is_taken_off_before_the_table_is_asked() {
        assert_eq!(TIMING.of("x64.add_rr_32"), TIMING.of("add_rr_32"));
    }

    #[test]
    fn a_multiply_costs_more_than_an_addition_and_wants_a_unit_there_is_one_of() {
        let add = TIMING.of("x64.add_rr_32").expect("described");
        let imul = TIMING.of("x64.imul_rr_32").expect("described");
        assert!(imul.latency > add.latency, "{imul:?} against {add:?}");
        assert_eq!(imul.unit, Unit::Mul);
        assert_eq!(TIMING.slots(Unit::Mul), 1);
        assert_eq!(TIMING.slots(Unit::Int), 4);
    }

    #[test]
    fn a_divide_costs_more_the_wider_it_is() {
        let narrow = TIMING.of("x64.idiv_quo_32").expect("described").latency;
        let wide = TIMING.of("x64.idiv_quo_64").expect("described").latency;
        assert!(wide > narrow, "{wide} against {narrow}");
        assert_eq!(TIMING.of("x64.div_rem_64").expect("described").unit, Unit::Div);
    }

    #[test]
    fn an_instruction_that_reads_memory_costs_its_own_work_and_a_load() {
        let add = TIMING.of("x64.add_rr_32").expect("described");
        let from_memory = TIMING.of("x64.add_rm_32").expect("described");
        assert_eq!(from_memory.latency, add.latency + LOAD);
        assert_eq!(from_memory.unit, Unit::Load);
        assert_eq!(TIMING.of("x64.imul_rm_32").expect("described").latency, 3 + LOAD);
    }

    #[test]
    fn an_instruction_that_encodes_to_nothing_takes_no_time_and_no_unit() {
        for name in ["x64.arg_val_32", "x64.ret_val_64", "x64.br_cond_8", "x64.arg_val_f64"] {
            let timing = TIMING.of(name).expect("described");
            assert_eq!(timing.latency, 0, "{name}");
            assert_eq!(timing.unit, Unit::Free, "{name}");
        }
    }

    #[test]
    fn a_floating_point_divide_is_on_the_divider_and_an_addition_is_not() {
        assert_eq!(TIMING.of("x64.divsd_rr").expect("described").unit, Unit::FloatDiv);
        assert_eq!(TIMING.of("x64.addsd_rr").expect("described").unit, Unit::Float);
        assert!(
            TIMING.of("x64.divsd_rr").expect("described").latency
                > TIMING.of("x64.divss_rr").expect("described").latency
        );
    }

    #[test]
    fn a_move_of_bits_between_the_files_is_not_priced_as_a_conversion() {
        let moved = TIMING.of("x64.movq_to_xmm").expect("described");
        let converted = TIMING.of("x64.cvtsi2sd_64").expect("described");
        assert!(moved.latency < converted.latency, "{moved:?} against {converted:?}");
        assert_eq!(TIMING.of("x64.movd_from_xmm"), Some(moved));
    }

    #[test]
    fn this_model_says_it_is_not_cycle_accurate() {
        assert!(!TIMING.accurate, "no target here has an automaton and none should claim one");
        assert!(!TIMING.model.is_empty(), "a model has to say which processor it describes");
    }

    #[test]
    fn the_instructions_a_schedule_must_not_move_are_the_ones_the_model_does_not_describe() {
        for name in ["x64.mfence", "x64.ud2", "x64.endbr64", "x64.nop", "x64.pause", "x64.fldcw"] {
            assert_eq!(TIMING.of(name).expect("described").unit, Unit::Fixed, "{name}");
        }
        for name in ["x64.add_rr_32", "x64.mov_rm_64", "x64.arg_val_32"] {
            let timing = TIMING.of(name).expect("described");
            assert_ne!(timing.unit, Unit::Fixed, "{name}");
        }
    }

    #[test]
    fn every_unit_has_at_least_one_of_it() {
        for &unit in Unit::ALL {
            assert!(TIMING.slots(unit) >= 1, "{unit:?}");
        }
    }
}
