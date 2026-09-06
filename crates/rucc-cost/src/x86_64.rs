//! The two cost tables for x86-64, and what the target answers about tuning.
//!
//! One file per target, per section 40.12, and this is the only one because x86-64 is the only back
//! end rucc has. Two tables, per section 40.3, because optimizing for size is a different table
//! rather than a weighting applied to the speed one.
//!
//! # What the numbers mean in each table
//!
//! In the speed table a [`Cycles`] is what it says: latency, in multiples of one register to register
//! add, on a recent out of order core. Where a number varies across microarchitectures it is the
//! one that is true of more of them, and where the variation is the whole point the fact is a
//! [`TuneFlag`] instead.
//!
//! In the size table a [`Cycles`] is not a time at all. It is the encoded length of the instruction, in
//! units of two bytes, which is exactly what GCC does: `COSTS_N_BYTES(N)` at
//! `gcc/config/i386/i386.h` expands to `(N) * 2` against a `COSTS_N_INSNS(N)` of `(N) * 4`, so a
//! two byte instruction and one simple operation are the same number. Reusing the field rather
//! than adding a parallel one is deliberate. Every pass asks the same question of whichever table
//! it was handed, and a pass that had to know which unit it was reading is a pass that would
//! eventually read the wrong one.
//!
//! The [`Bytes`] field is a real byte count in both tables, because the thing it describes, the
//! narrowest store worth emitting, is a fact about store forwarding and not about the goal.
//!
//! # Where the speed numbers come from
//!
//! Published latencies for Intel Skylake and later and AMD Zen 2 and later, which is the range of
//! machines a generic x86-64 build has to be reasonable on. They are latencies rather than
//! reciprocal throughputs because the cost model prices a dependence chain, and the places where
//! throughput is what matters, the reassociation width and the block copy threshold, have their own
//! fields.

use crate::heuristics;
use crate::{Bytes, CostTable, Cycles, Goal, TargetCosts, TuneFlag, Tuning};
use std::sync::LazyLock;

/// An encoded length, in the units the size table is written in.
///
/// GCC's `COSTS_N_BYTES`. Two bytes is one unit, so a size table entry can be read straight off an
/// instruction encoding without anybody converting in their head.
const fn bytes(n: i64) -> Cycles {
    Cycles::hundredths(n * (Cycles::SCALE / 2))
}

/// Costs on a recent out of order x86-64 core, in cycles.
static SPEED: LazyLock<CostTable> = LazyLock::new(|| {
    CostTable::builder()
        // The unit. One cycle, four ports wide, on everything since Sandy Bridge.
        .add(Cycles::ONE)
        // Also one cycle, and that is why `TuneFlag::PreferLea` is off. `lea` was worth preferring
        // when it saved a flag clobber on a machine with partial flag stalls, and on a machine
        // where an add is available on twice as many ports it is worth the opposite.
        .lea(Cycles::ONE)
        .shift_const(Cycles::ONE)
        // Two, not one. A shift by a register on x86-64 reads `cl` and writes flags conditionally
        // on the count being zero, and the second of those is why it is not a one cycle operation
        // on any core that does not have `bmi2`.
        .shift_var(Cycles::insns(2))
        // `imul` latency by width. The 32-bit form is the fast one at three cycles, the 64-bit
        // form is a cycle or two behind it, and the byte and word forms are slower still because
        // they write a partial register.
        .mult([Cycles::insns(4), Cycles::insns(4), Cycles::insns(3), Cycles::insns(5)])
        // What one more set bit costs when a constant multiply is expanded into shifts and adds.
        // One, because that is what the extra shift and add cost. GCC's generic table says zero
        // here, which is right for its expander and not for one that prices what it emits.
        .mult_bit(Cycles::ONE)
        // `idiv`, the most expensive integer instruction on the machine by an order of magnitude,
        // not pipelined, and the reason it is worth a great deal of trouble to turn a division by
        // a constant into a multiply.
        .divide([Cycles::insns(20), Cycles::insns(20), Cycles::insns(26), Cycles::insns(42)])
        .movsx(Cycles::ONE)
        // The 32 to 64 zero extension is free because writing a 32-bit register zeroes the top
        // half, and this number is for the ones that are not that, which are ordinary moves.
        .movzx(Cycles::ONE)
        .reg_move(Cycles::ONE)
        // L1 hit latency, the same for every integer width.
        .move_int_load([Cycles::insns(4); 4])
        // A store retires into the store buffer, so what it costs the code around it is the
        // address computation and the port, not the write.
        .move_int_store([Cycles::ONE; 4])
        .move_int_reg(Cycles::ONE)
        // An SSE load is a cycle behind an integer load of the same address.
        .move_fp_load([Cycles::insns(5); 2])
        .move_fp_store([Cycles::ONE; 2])
        // `movaps` between two xmm registers is renamed on Intel and one cycle on AMD.
        .move_fp_reg(Cycles::ONE)
        // `movd` and `movq` cross a domain boundary, which is the two or three cycles this is.
        .move_fp_to_int(Cycles::insns(3))
        .move_int_to_fp(Cycles::insns(3))
        // Every one of the five modes exists on x86-64, so nothing here is infinite, and the
        // discrimination between them is almost entirely complexity rather than cycles. The one
        // real difference is that a load whose address uses an index does not qualify for the
        // fast path Intel calls simple addressing, and pays a cycle for it.
        .addr([Cycles::ZERO, Cycles::ZERO, Cycles::ONE, Cycles::ONE, Cycles::ONE])
        // What an unpredictable branch costs. Not the mispredict penalty, which is below: this is
        // what the branch is worth to remove before the penalty is weighed, and three is the
        // compare, the jump, and the fetch bubble.
        .branch_cost(Cycles::insns(3))
        // A mispredict is about twenty cycles on every core in range, and it has been about twenty
        // cycles for fifteen years, because it is the length of the pipeline in front of execute.
        .mispredict_penalty(Cycles::insns(20))
        .move_ratio(heuristics::BLOCK_COPY_MOVES_FOR_SPEED)
        .clear_ratio(heuristics::BLOCK_COPY_MOVES_FOR_SPEED)
        .cheapest_store(CHEAPEST_STORE)
        // Two independent integer chains, because the machine has four ALU ports and a
        // reassociated tree needs a register per branch of it. Four would use the ports and lose
        // the registers.
        .reassoc_int(2)
        // Four for floating point, where the latencies are long enough that the tree wins even
        // paying for the extra live values.
        .reassoc_fp(4)
        .build()
});

/// The same target, costed by encoded length instead of by time.
static SIZE: LazyLock<CostTable> = LazyLock::new(|| {
    CostTable::builder()
        // Two bytes, an opcode and a modrm.
        .add(bytes(2))
        // Three, because `lea` always carries a modrm and usually a sib. This is the direction
        // opposite to the speed table, where the two tie, and it is the clearest small example of
        // why the two tables are two tables.
        .lea(bytes(3))
        .shift_const(bytes(3))
        .shift_var(bytes(2))
        .mult([bytes(3); 4])
        .mult_bit(Cycles::ZERO)
        // Three bytes, the same as a multiply. Optimizing for size, a division is one short
        // instruction and turning it into a multiply and two shifts to save twenty cycles is
        // exactly the trade `-Os` is asking not to make.
        .divide([bytes(3); 4])
        .movsx(bytes(3))
        .movzx(bytes(3))
        .reg_move(bytes(2))
        .move_int_load([bytes(2); 4])
        .move_int_store([bytes(2); 4])
        .move_int_reg(bytes(2))
        .move_fp_load([bytes(4); 2])
        .move_fp_store([bytes(4); 2])
        .move_fp_reg(bytes(4))
        .move_fp_to_int(bytes(4))
        .move_int_to_fp(bytes(4))
        // A displacement is a byte or four, an index needs a sib byte, and a scale is free once
        // the sib is there. Costed as one byte for a short displacement rather than four, because
        // the addresses a pass is choosing between are mostly frame offsets.
        .addr([Cycles::ZERO, bytes(1), bytes(1), bytes(1), bytes(2)])
        // Section 40.5's number, taken from the same place the speed table's is not.
        .branch_cost(heuristics::BRANCH_COST_FOR_SIZE)
        // Identical to the speed table, and it should be. How long the pipeline is does not depend
        // on which flag the compiler was invoked with, and section 40.13 asks only that the two
        // tables never disagree about what is possible, not that they disagree about everything.
        .mispredict_penalty(Cycles::insns(20))
        .move_ratio(heuristics::BLOCK_COPY_MOVES_FOR_SIZE)
        .clear_ratio(heuristics::BLOCK_COPY_MOVES_FOR_SIZE)
        .cheapest_store(CHEAPEST_STORE)
        // No reassociation. A tree is the same instruction count as a chain, so it wins nothing
        // here, and it holds more values live, so it loses whatever the spills cost.
        .reassoc_int(heuristics::REASSOC_WIDTH_UNTUNED)
        .reassoc_fp(heuristics::REASSOC_WIDTH_UNTUNED)
        .build()
});

/// The narrowest store worth emitting on x86-64.
///
/// Four bytes. Section 40.7 wants a partially dead store trimmed only down to a width the target is
/// happy with, and on x86-64 the thing that stops it going further is store to load forwarding: a
/// one byte store followed by a four byte load that covers it does not forward and costs the round
/// trip through the cache, which is more than the three dead bytes were worth.
const CHEAPEST_STORE: Bytes = Bytes(4);

/// What this target answers about the decisions that are booleans, per section 40.4.
const TUNING: Tuning = Tuning::untuned()
    .with(TuneFlag::Schedule)
    .with(TuneFlag::FastUnalignedAccess)
    .with(TuneFlag::FastMultiply);

/// The x86-64 cost model.
struct X86_64;

impl TargetCosts for X86_64 {
    fn table(&self, goal: Goal) -> &CostTable {
        match goal {
            Goal::Speed => &SPEED,
            Goal::Size => &SIZE,
        }
    }

    fn tune(&self, flag: TuneFlag) -> bool {
        TUNING.get(flag)
    }

    fn name(&self) -> &'static str {
        "x86-64"
    }
}

/// The costs for x86-64, which is what [`crate::for_arch`] hands out.
pub static COSTS: &(dyn TargetCosts + 'static) = &X86_64;

#[cfg(test)]
mod tests {
    use super::{COSTS, SIZE, SPEED, bytes};
    use crate::table::{AddrMode, Width};
    use crate::{Cost, Cycles, Goal, TuneFlag, heuristics};

    #[test]
    fn both_tables_are_complete_or_neither_exists() {
        // Building them is the check, per the builder in `table.rs`, so forcing both is the test.
        // If a field were missing this would panic naming it rather than producing a zero.
        assert!(!SPEED.add.is_infinite());
        assert!(!SIZE.add.is_infinite());
    }

    #[test]
    fn the_two_tables_agree_about_what_the_machine_can_do() {
        // Section 40.13: "The two tables must differ only in numbers, never in capability, and
        // that is checkable." This is that check. If the speed table said x86-64 has a scaled
        // index mode and the size table said it does not, selection and costing would disagree
        // about what is legal and the failure would show up as unexplained code at `-Os` only.
        let speed = SPEED.capabilities();
        let size = SIZE.capabilities();
        assert_eq!(speed.len(), size.len());
        for ((name, from_speed), (also, from_size)) in speed.iter().zip(size.iter()) {
            assert_eq!(name, also);
            assert_eq!(from_speed, from_size, "the two tables disagree about `{name}`");
        }
    }

    #[test]
    fn the_two_tables_are_not_the_same_table() {
        // The other half of the previous test. Two tables that agree about everything would mean
        // `-Os` is not doing anything, and the pair would be an elaborate way to have one table.
        assert_ne!(*SPEED, *SIZE);
        // The clearest single case: a divide is twenty six cycles and three bytes, so a pass that
        // wants to expand it is right for speed and wrong for size.
        assert!(SPEED.divide_of(Width::W32) > SPEED.mult_of(Width::W32));
        assert_eq!(SIZE.divide_of(Width::W32), SIZE.mult_of(Width::W32));
    }

    #[test]
    fn a_divide_is_the_expensive_one_at_every_width() {
        for width in Width::ALL {
            assert!(
                SPEED.divide_of(width) > SPEED.mult_of(width),
                "a divide is not dearer than a multiply at {} bits",
                width.bits()
            );
            assert!(SPEED.mult_of(width) > SPEED.add);
        }
    }

    #[test]
    fn every_addressing_mode_exists_on_this_target() {
        // x86-64 has all five, so nothing in the `addr` field is infinite, and the tiebreak
        // between them is complexity. That in turn means section 40.9's refinement, which drops a
        // point of complexity when the target has no unscaled index mode, must not fire here.
        for mode in AddrMode::ALL {
            assert!(SPEED.has_addr(mode), "{mode:?} should exist on x86-64");
            assert_eq!(SPEED.addr_cost(mode).complexity, mode.complexity());
        }
        assert_eq!(SPEED.addr_cost(AddrMode::Base), Cost::new(Cycles::ZERO, 0));
    }

    #[test]
    fn an_index_costs_a_cycle_and_a_displacement_does_not() {
        // The Intel simple addressing rule: a load off a base and a small displacement hits L1 in
        // four cycles and anything with an index takes five.
        assert_eq!(SPEED.addr[AddrMode::BaseDisp.index()], Cycles::ZERO);
        assert_eq!(SPEED.addr[AddrMode::BaseIndex.index()], Cycles::ONE);
    }

    #[test]
    fn a_lea_ties_with_an_add_for_speed_and_loses_for_size() {
        // Why `TuneFlag::PreferLea` is off, in two lines. It is also the case that makes the two
        // tables worth having, because a single weighted table cannot express a tie in one
        // direction and a loss in the other.
        assert_eq!(SPEED.lea, SPEED.add);
        assert!(SIZE.lea > SIZE.add);
    }

    #[test]
    fn a_branch_is_free_when_predicted_and_costs_the_table_otherwise() {
        assert_eq!(COSTS.branch_cost(Goal::Speed, true), heuristics::BRANCH_COST_PREDICTABLE);
        assert_eq!(COSTS.branch_cost(Goal::Speed, false), SPEED.branch_cost);
        assert_eq!(COSTS.branch_cost(Goal::Size, false), heuristics::BRANCH_COST_FOR_SIZE);
        // Optimizing for size, a branch costs the same whether or not it is predicted, because
        // what is being counted is bytes and the branch predictor does not change how many there
        // are.
        assert_eq!(COSTS.branch_cost(Goal::Size, true), COSTS.branch_cost(Goal::Size, false));
    }

    #[test]
    fn a_mispredict_is_worth_far_more_than_the_branch_it_came_from() {
        // The relationship section 40.10 turns a switch lowering decision on. If these two ever
        // came out close, an indirect branch with many targets would be priced as an ordinary
        // branch and every switch would become a jump table.
        assert!(SPEED.mispredict_penalty > SPEED.branch_cost * 4);
    }

    #[test]
    fn the_size_table_reads_in_bytes() {
        assert_eq!(bytes(2), Cycles::ONE);
        assert_eq!(SIZE.add, bytes(2));
        // A whole number of bytes is not a whole number of units, and the fixed point is what
        // makes that expressible rather than rounded away.
        assert_eq!(bytes(3), Cycles::hundredths(150));
    }

    #[test]
    fn the_target_answers_the_tuning_flags_it_has_reasons_for() {
        assert!(COSTS.tune(TuneFlag::Schedule));
        assert!(COSTS.tune(TuneFlag::FastUnalignedAccess));
        assert!(COSTS.tune(TuneFlag::FastMultiply));
        // Off, and each for a reason written where the flag is declared. `PreferLea` because an
        // add is available on more ports, `IfConvertPredictable` because a predicted branch is
        // already free, and `PartialRegisterStall` because no core in range has the stall.
        assert!(!COSTS.tune(TuneFlag::PreferLea));
        assert!(!COSTS.tune(TuneFlag::IfConvertPredictable));
        assert!(!COSTS.tune(TuneFlag::PartialRegisterStall));
    }

    #[test]
    fn a_block_copy_gets_fewer_moves_when_optimizing_for_size() {
        assert_eq!(SPEED.move_ratio, heuristics::BLOCK_COPY_MOVES_FOR_SPEED);
        assert_eq!(SIZE.move_ratio, heuristics::BLOCK_COPY_MOVES_FOR_SIZE);
        assert!(SPEED.move_ratio > SIZE.move_ratio);
    }

    #[test]
    fn nothing_in_either_table_is_free_unless_it_really_is() {
        // A zero cost is the failure section 40.13 is about, so the zeros that are here should be
        // few enough to name. There are exactly three kinds: the base addressing mode, which is
        // the operand every load has anyway; the other addressing modes that fold into an
        // instruction without lengthening it; and `mult_bit` in the size table, where a set bit in
        // a constant multiplier does not change how long the `imul` encoding is.
        assert_eq!(SPEED.add, Cycles::ONE);
        assert!(SPEED.reg_move > Cycles::ZERO);
        assert!(SIZE.reg_move > Cycles::ZERO);
        for width in Width::ALL {
            assert!(SPEED.int_load(width) > Cycles::ZERO);
            assert!(SPEED.int_store(width) > Cycles::ZERO);
            assert!(SIZE.int_load(width) > Cycles::ZERO);
        }
    }
}
