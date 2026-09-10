//! How much of the frame the register allocator had to use, function by function.
//!
//! Design: `spec/safe-memory/13-performance.md` section 13.1, whose table of metrics has a row for
//! spill and fill counts, and section 13.2.1, which says why: a capability in flight is four words
//! in registers, and if materializing one pushes something else onto the stack in a hot loop then
//! no amount of check elimination saves us and the representation is what has to change. Milestone
//! S4 in `spec/safe-memory/16-milestones.md` asks for the delta on the pointer heavy benchmarks,
//! and `cargo xtask pressure` is what reads these numbers back.
//!
//! # What is counted
//!
//! Three numbers per function. The slots are how many values the allocator could not keep in a
//! register at all, which is what the frame grows by. The stores are how many times one of them is
//! written to its slot and the reloads are how many times one is read back, which is what the
//! program pays at run time and is not the same number: a value spilled once and read in a loop
//! costs one store and as many reloads as the loop has instructions that want it.
//!
//! A move from one slot to another counts as both, because it is both. That happens on an edge
//! carrying a spilled value into a parameter that was itself spilled, and no machine here has an
//! instruction for it, so it goes through a scratch register and really is a load and a store.
//!
//! # What the numbers are not
//!
//! Not a claim about the best allocator we could have. There is one allocator in this compiler and
//! it is the single pass one `spec/10-backend.md` section 10.4 describes, so a function that spills
//! here might not spill under the backtracking allocator M4 brings. What the delta between two
//! builds of the same program says is how much more pressure the instrumented one puts on whatever
//! allocator is reading it, and that comparison is fair as long as both sides go through the same
//! one.

use std::fmt::Write as _;

use rucc_regalloc::Allocation;
use rucc_regalloc::assign::Place;

/// What allocating one function cost.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Cost {
    /// Values that went to the stack, which is what the frame grows by.
    pub slots: usize,
    /// Writes into a slot.
    pub stores: usize,
    /// Reads out of a slot.
    pub reloads: usize,
}

impl Cost {
    /// What one allocation came to.
    #[must_use]
    pub fn of(allocation: &Allocation) -> Self {
        let mut cost = Self { slots: allocation.assignment.spilled(), ..Self::default() };
        for edit in &allocation.edits {
            if matches!(edit.mov.to, Place::Slot(_)) {
                cost.stores += 1;
            }
            if matches!(edit.mov.from, Place::Slot(_)) {
                cost.reloads += 1;
            }
        }
        cost
    }

    /// Takes in another one, which is how a whole module or a whole command line is added up.
    fn add(&mut self, other: Self) {
        self.slots += other.slots;
        self.stores += other.stores;
        self.reloads += other.reloads;
    }
}

/// One function, and what it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    /// What the function is called, which is the symbol name and not the name in the source.
    name: String,
    /// What allocating it came to.
    cost: Cost,
}

/// What every function a run allocated cost, in the order they were allocated.
///
/// Kept per function rather than as one total, because the number that matters is a hot loop and
/// the way to find one in a file the size of an amalgamation is to sort the rows. The total is
/// there too, since it is what a comparison of two builds is usually about and nobody should have
/// to add up ten thousand lines to get it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Pressure {
    /// One per function, in the order they came through.
    rows: Vec<Row>,
}

impl Pressure {
    /// Nothing recorded yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes down what one function cost.
    pub fn record(&mut self, name: &str, cost: Cost) {
        self.rows.push(Row { name: name.to_owned(), cost });
    }

    /// Takes in everything another one recorded, which is how one file's answer joins a run's.
    pub fn merge(&mut self, other: &Self) {
        self.rows.extend(other.rows.iter().cloned());
    }

    /// How many functions were allocated.
    #[must_use]
    pub fn functions(&self) -> usize {
        self.rows.len()
    }

    /// Every function's cost added together.
    #[must_use]
    pub fn total(&self) -> Cost {
        let mut total = Cost::default();
        for row in &self.rows {
            total.add(row.cost);
        }
        total
    }

    /// What `-Zregister-pressure=FILE` writes.
    ///
    /// A comment holding the totals and then one line per function, each of them the three counts
    /// and then the name. The counts come first because they are the fields a reader is sorting
    /// on and the name is the one field that could be any length, which is the layout
    /// `-Zrule-coverage` uses for the same reason.
    ///
    /// Every function is listed, including the ones that spilled nothing, so that one of these
    /// files says how much of the module was measured as well as what the answer was. A build that
    /// stopped early and a build that spilled nowhere would otherwise look the same.
    #[must_use]
    pub fn listing(&self) -> String {
        let total = self.total();
        let mut out = format!(
            "# rucc register pressure: {} functions, {} slots, {} stores, {} reloads\n",
            self.rows.len(),
            total.slots,
            total.stores,
            total.reloads
        );
        for row in &self.rows {
            let _ = writeln!(
                out,
                "{} {} {} {}",
                row.cost.slots, row.cost.stores, row.cost.reloads, row.name
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{Func, Opcode};
    use rucc_regalloc::assign::{Assignment, Env};
    use rucc_regalloc::moves::Move;
    use rucc_regalloc::rewrite::{At, Edit};
    use rucc_target::x86_64::{GPR, SYSV};

    use super::*;

    /// An allocation holding the moves given and nothing else, which is all the counting reads.
    fn allocated(moves: &[(Place, Place)]) -> Allocation {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let edits = moves
            .iter()
            .map(|&(to, from)| Edit {
                at: At::StartOf(block),
                mov: Move::new(to, from),
                class: GPR,
            })
            .collect();
        Allocation { assignment: Assignment::empty(0), edits }
    }

    #[test]
    fn a_write_into_a_slot_is_a_store_and_a_read_out_of_one_is_a_reload() {
        // The two are counted apart because they are different costs: a value spilled once and
        // read in a loop stores once and reloads every time round.
        let reg = Place::Reg(SYSV.int_order[0]);
        let cost = Cost::of(&allocated(&[
            (Place::Slot(0), reg),
            (reg, Place::Slot(0)),
            (reg, Place::Slot(0)),
        ]));
        assert_eq!(cost.stores, 1);
        assert_eq!(cost.reloads, 2);
    }

    #[test]
    fn a_move_from_one_slot_to_another_is_both() {
        // It goes through a scratch register, because no machine here has memory to memory, so
        // the program really does pay for a load and a store.
        let cost = Cost::of(&allocated(&[(Place::Slot(1), Place::Slot(0))]));
        assert_eq!(cost.stores, 1);
        assert_eq!(cost.reloads, 1);
    }

    #[test]
    fn the_listing_holds_every_function_and_the_totals_are_the_sum_of_them() {
        let mut pressure = Pressure::new();
        pressure.record("f", Cost { slots: 2, stores: 3, reloads: 4 });
        pressure.record("g", Cost::default());
        let mut second = Pressure::new();
        second.record("h", Cost { slots: 1, stores: 1, reloads: 5 });
        pressure.merge(&second);

        assert_eq!(pressure.functions(), 3);
        assert_eq!(pressure.total(), Cost { slots: 3, stores: 4, reloads: 9 });

        let listing = pressure.listing();
        let lines: Vec<&str> = listing.lines().collect();
        assert_eq!(lines.len(), 4, "{listing}");
        assert!(lines[0].contains("3 functions, 3 slots, 4 stores, 9 reloads"), "{}", lines[0]);
        assert_eq!(lines[1], "2 3 4 f");
        // The function that spilled nothing is listed too, so the file says how much was measured.
        assert_eq!(lines[2], "0 0 0 g");
        assert_eq!(lines[3], "1 1 5 h");
    }

    #[test]
    fn a_function_that_runs_out_of_registers_is_recorded_as_having_spilled() {
        // End to end through the allocator rather than through a made up edit list, so that the
        // three counts are the ones a real allocation produces.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        let third = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        func.build(block, opcode).def(third, GPR).finish();
        func.build(block, opcode).uses(first, GPR).uses(second, GPR).uses(third, GPR).finish();

        // Two registers to hand out and three values all wanted at once, so one goes to the stack.
        let env = Env::new().with(GPR, &SYSV.int_order[..2], &SYSV.int_order[2..5]);
        let cost = Cost::of(&rucc_regalloc::run(&mut func, &env, "f"));
        assert_eq!(cost.slots, 1);
        assert_eq!(cost.stores, 1);
        assert_eq!(cost.reloads, 1);
    }
}
