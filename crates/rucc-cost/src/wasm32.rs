//! The two cost tables for 32-bit WebAssembly, and what the target answers about tuning.
//!
//! The second target with a file of its own, per section 40.12. Section 7.4 of the wasm plan
//! (`Spec/2131/platform/wasm/07-control-flow.md`) asks for wasm numbers in this crate. A pass that
//! needs a price leaves a module alone on a target with no table, and on wasm32 that included the
//! ivopts pass of `rucc-opt` (tamnd/rucc#3262).
//!
//! # What the numbers mean in each table
//!
//! A wasm module does not run as written. An engine compiles each function to machine code for the
//! host before it runs, and almost every operator becomes one host instruction, or a few. So the
//! speed table is the host's table, in the same units as `crate::x86_64`, and x86-64 is the host it
//! is written for, because it is the most common one and the one with the fewest registers. Where
//! the engine adds work that a native compiler does not, the number says so.
//!
//! The size table counts bytes of the code section, one unit for each byte. An operator is one
//! byte, and an immediate is one byte or more in LEB128. A unit of one byte for one operator keeps
//! an add at [`Cycles::ONE`] in both tables, which is what every threshold in
//! [`crate::heuristics`] is written against. What is counted is the operator and its immediates.
//! The `local.get` of each operand is the same for every operator and is not counted, because it
//! cannot change which of two choices is smaller.
//!
//! # The addresses
//!
//! This is the fact the table is for. A load or a store takes one address from the stack and adds
//! an unsigned constant from its offset field, with no wrap. There is no index and no scale. So an
//! address with an index is two more operators in front of the load, an add and usually a shift,
//! and that is a cost no x86-64 or AArch64 table has. The three modes with an index are
//! [`Cycles::INFINITE`], which is how a table says that a mode does not exist.

use crate::heuristics;
use crate::{Bytes, CostTable, Cycles, Goal, RegClass, TargetCosts, TuneFlag, Tuning};
use std::sync::LazyLock;

/// Costs on the host, in cycles, of the code an engine makes from each operator.
static SPEED: LazyLock<CostTable> = LazyLock::new(|| {
    CostTable::builder()
        .add(Cycles::ONE)
        // wasm has no `lea`. An address computation is an `i32.add`.
        .lea(Cycles::ONE)
        .shift_const(Cycles::ONE)
        // One. The amount of a wasm shift is taken modulo the width, which is what the shift of
        // x86-64 and AArch64 does too, so the engine emits the shift and nothing more.
        .shift_var(Cycles::ONE)
        // The host's multiply. The 8 and 16 bit entries are the 32-bit multiply, because wasm
        // has no narrower one and a narrow value is computed in an `i32`.
        .mult([Cycles::insns(3), Cycles::insns(3), Cycles::insns(3), Cycles::insns(4)])
        .mult_bit(Cycles::ONE)
        // The host's divide, and one more for the test against zero that the engine puts in front
        // of each one, because a wasm division by zero traps whatever the host does.
        .divide([Cycles::insns(21), Cycles::insns(21), Cycles::insns(27), Cycles::insns(43)])
        .movsx(Cycles::ONE)
        .movzx(Cycles::ONE)
        .reg_move(Cycles::ONE)
        // An L1 hit. The engine adds the base of the linear memory to the address, and on a 64-bit
        // host the add is part of the addressing mode, so the load costs what a native load costs.
        .move_int_load([Cycles::insns(4); 4])
        .move_int_store([Cycles::ONE; 4])
        .move_int_reg(Cycles::ONE)
        .move_fp_load([Cycles::insns(5); 2])
        .move_fp_store([Cycles::ONE; 2])
        .move_fp_reg(Cycles::ONE)
        .move_fp_to_int(Cycles::insns(3))
        .move_int_to_fp(Cycles::insns(3))
        // A base and an offset, and nothing else. See the module documentation.
        .addr([Cycles::ZERO, Cycles::ZERO, Cycles::INFINITE, Cycles::INFINITE, Cycles::INFINITE])
        // `i32.lt_s` and its operands, an operation like any other.
        .compare_reg(Cycles::ONE)
        // Nothing. `br_if` branches on a value that is not zero, so a loop that counts down to
        // zero gives the counter to the branch and has no comparison at all.
        .compare_zero(Cycles::ZERO)
        // The host's branch, which is what a `br_if` becomes.
        .branch_cost(Cycles::insns(3))
        .mispredict_penalty(Cycles::insns(20))
        .move_ratio(crate::param!(heuristics::BLOCK_COPY_MOVES_FOR_SPEED))
        .clear_ratio(crate::param!(heuristics::BLOCK_COPY_MOVES_FOR_SPEED))
        .cheapest_store(CHEAPEST_STORE)
        .reassoc_int(2)
        .reassoc_fp(4)
        .build()
});

/// The same target, costed in bytes of the code section, one unit for each byte.
static SIZE: LazyLock<CostTable> = LazyLock::new(|| {
    CostTable::builder()
        // `i32.add`, one byte.
        .add(Cycles::ONE)
        .lea(Cycles::ONE)
        // `i32.shl` and an `i32.const` where an add has a `local.get`. The two are the same size
        // for a count below 64, so a shift by a constant ties with an add.
        .shift_const(Cycles::ONE)
        .shift_var(Cycles::ONE)
        .mult([Cycles::ONE; 4])
        .mult_bit(Cycles::ZERO)
        // One byte, the same as a multiply. Optimizing for size, a division by a constant stays a
        // division.
        .divide([Cycles::ONE; 4])
        .movsx(Cycles::ONE)
        .movzx(Cycles::ONE)
        // A `local.get` and a `local.set`, two bytes each.
        .reg_move(Cycles::insns(4))
        // The operator, the alignment and an offset of one byte.
        .move_int_load([Cycles::insns(3); 4])
        .move_int_store([Cycles::insns(3); 4])
        .move_int_reg(Cycles::insns(4))
        .move_fp_load([Cycles::insns(3); 2])
        .move_fp_store([Cycles::insns(3); 2])
        .move_fp_reg(Cycles::insns(4))
        // `i32.reinterpret_f32` and the three others like it are one byte each.
        .move_fp_to_int(Cycles::ONE)
        .move_int_to_fp(Cycles::ONE)
        // An offset of 128 or more is a second byte of LEB128, and the offsets a pass chooses
        // between are mostly small, so a displacement costs nothing here.
        .addr([Cycles::ZERO, Cycles::ZERO, Cycles::INFINITE, Cycles::INFINITE, Cycles::INFINITE])
        .compare_reg(Cycles::ONE)
        .compare_zero(Cycles::ZERO)
        // `br_if` and its depth, two bytes, which is what section 40.5 says for every target.
        .branch_cost(crate::param!(heuristics::BRANCH_COST_FOR_SIZE))
        .mispredict_penalty(Cycles::insns(20))
        .move_ratio(crate::param!(heuristics::BLOCK_COPY_MOVES_FOR_SIZE))
        .clear_ratio(crate::param!(heuristics::BLOCK_COPY_MOVES_FOR_SIZE))
        .cheapest_store(CHEAPEST_STORE)
        .reassoc_int(crate::param!(heuristics::REASSOC_WIDTH_UNTUNED))
        .reassoc_fp(crate::param!(heuristics::REASSOC_WIDTH_UNTUNED))
        .build()
});

/// The narrowest store worth emitting, which is the host's: four bytes, for store forwarding.
const CHEAPEST_STORE: Bytes = Bytes(4);

/// What this target answers about the decisions that are booleans, per section 40.4.
///
/// Not `Schedule`. The engine schedules the code it makes, and the order of the operators on the
/// stack is not the order of the host's instructions. Unaligned access is allowed by the format and
/// is fast on every host an engine runs on, and so is a multiply.
const TUNING: Tuning =
    Tuning::untuned().with(TuneFlag::FastUnalignedAccess).with(TuneFlag::FastMultiply);

/// How many integer registers the engine's allocator hands out on an x86-64 host.
///
/// Twelve, the same as `crate::x86_64` hands out. A wasm function has as many locals as it names,
/// but the engine puts them in the host's registers, and a value the host has no register for is
/// a spill in the engine's code. An engine also keeps the base of the linear memory in a register
/// on most hosts, so this is one more than it can give, and AArch64 hosts have more.
const ALLOCATABLE_GPR: u32 = 12;

/// How many vector registers the engine's allocator hands out, by the same reasoning.
const ALLOCATABLE_VECTOR: u32 = 14;

/// The wasm32 cost model.
struct Wasm32;

impl TargetCosts for Wasm32 {
    fn table(&self, goal: Goal) -> &CostTable {
        match goal {
            Goal::Speed => &SPEED,
            Goal::Size => &SIZE,
        }
    }

    fn tune(&self, flag: TuneFlag) -> bool {
        TUNING.get(flag)
    }

    fn allocatable(&self, class: RegClass) -> u32 {
        match class {
            RegClass::Integer => ALLOCATABLE_GPR,
            RegClass::Float => ALLOCATABLE_VECTOR,
        }
    }

    fn name(&self) -> &'static str {
        "wasm32"
    }
}

/// The costs for wasm32, which is what [`crate::for_arch`] hands out.
pub static COSTS: &(dyn TargetCosts + 'static) = &Wasm32;

#[cfg(test)]
mod tests {
    use super::{COSTS, SIZE, SPEED};
    use crate::table::{AddrMode, Width};
    use crate::{Cycles, Goal, TuneFlag, heuristics};

    #[test]
    fn both_tables_are_complete() {
        // Building them is the check, per the builder in `table.rs`.
        assert!(!SPEED.add.is_infinite());
        assert!(!SIZE.add.is_infinite());
    }

    #[test]
    fn the_two_tables_agree_about_what_the_machine_can_do() {
        let speed = SPEED.capabilities();
        let size = SIZE.capabilities();
        assert_eq!(speed.len(), size.len());
        for ((name, from_speed), (also, from_size)) in speed.iter().zip(size.iter()) {
            assert_eq!(name, also);
            assert_eq!(from_speed, from_size, "the two tables disagree about `{name}`");
        }
    }

    #[test]
    fn an_address_is_a_base_and_an_offset_and_nothing_else() {
        assert!(SPEED.has_addr(AddrMode::Base));
        assert!(SPEED.has_addr(AddrMode::BaseDisp));
        for mode in [AddrMode::BaseIndex, AddrMode::BaseIndexScale, AddrMode::BaseIndexScaleDisp] {
            assert!(!SPEED.has_addr(mode), "{mode:?} does not exist on wasm32");
            assert!(!SIZE.has_addr(mode), "{mode:?} does not exist on wasm32");
        }
        assert_eq!(SPEED.addr[AddrMode::BaseDisp.index()], Cycles::ZERO);
    }

    #[test]
    fn an_add_is_the_unit_in_both_tables() {
        assert_eq!(SPEED.add, Cycles::ONE);
        assert_eq!(SIZE.add, Cycles::ONE);
        assert_eq!(SIZE.branch_cost, heuristics::BRANCH_COST_FOR_SIZE);
    }

    #[test]
    fn a_divide_is_dearer_than_a_multiply_for_speed_and_not_for_size() {
        for width in Width::ALL {
            assert!(SPEED.divide_of(width) > SPEED.mult_of(width));
            assert_eq!(SIZE.divide_of(width), SIZE.mult_of(width));
        }
    }

    #[test]
    fn a_countdown_has_no_comparison() {
        assert_eq!(SPEED.compare_zero, Cycles::ZERO);
        assert_eq!(SIZE.compare_zero, Cycles::ZERO);
    }

    #[test]
    fn the_target_answers_the_tuning_flags_it_has_reasons_for() {
        assert!(!COSTS.tune(TuneFlag::Schedule));
        assert!(COSTS.tune(TuneFlag::FastUnalignedAccess));
        assert!(COSTS.tune(TuneFlag::FastMultiply));
        assert!(!COSTS.tune(TuneFlag::PreferLea));
        assert_eq!(COSTS.name(), "wasm32");
        assert_eq!(COSTS.branch_cost(Goal::Speed, false), SPEED.branch_cost);
    }
}
