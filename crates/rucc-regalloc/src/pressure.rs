//! How many values want a register at each point of a function, and how many registers there are
//! to put them in.
//!
//! Design: `spec/optimizer/39-register-allocation.md` section 39.7, and tamnd/rucc#1177.
//!
//! This is the question the spill phase in [`crate::spill`] asks before anything is placed. A
//! point where more values are live than there are registers is a point where some of them have
//! to be in memory whatever the assignment does, and knowing that up front means the choice of
//! which ones can be made by what each costs, over the whole function, instead of by which value
//! the assignment happened to meet last.
//!
//! # What is counted
//!
//! A value is counted at every point [`crate::live`] says it is live at, in its own class. A value
//! an instruction can only read from memory is not counted, since it never wants a register. A
//! physical register an operand names is not counted either, because it is not a value the
//! allocator places.
//!
//! The room at a point is every register the environment hands out in the class, less the ones an
//! instruction takes outright there. A call is where that shows: at the point a call writes its
//! results it destroys every register the convention says it destroys, and only the ones it saves
//! are left for what lives across it.
//!
//! # Why it may say less than the truth and never more
//!
//! Both halves are counted so that the difference is a floor. A register a fixed operand claims
//! for one value is still counted as room, though nothing else may be in it there, and the answer
//! of a two address instruction is counted from where it is written rather than from where its
//! source is read. Each of those makes the room look larger or the values look fewer than they
//! are, so a point this says is over is really over, and it is over by at least as much as this
//! says. The spill phase leans on that: taking no more values off a point than this says have to
//! go is never taking off one that could have stayed for want of room there.

use rucc_mir::{Func, Reg};
use rucc_target::RegClass;

use crate::assign::{self, Blocks, Env};
use crate::live::{Area, Live};
use crate::order::{Order, Point};

/// How many values want a register at each point, and how many registers there are for them.
#[derive(Debug, Clone, Default)]
pub struct Pressure {
    /// By class number, then by point.
    wanted: Vec<Vec<u32>>,
    /// By class number, then by point.
    room: Vec<Vec<u32>>,
}

impl Pressure {
    /// Counts the values and the registers at every point of a function.
    ///
    /// # Panics
    ///
    /// Panics on a function with more points than a table can index, which no machine has the
    /// memory to have handed it.
    #[must_use]
    pub fn of(func: &Func, order: &Order, live: &Live, env: &Env) -> Self {
        Self::with(func, order, live, env, &assign::forced(func), &assign::blocked(func, order))
    }

    /// The same, over the values the instructions send to the stack and the registers they insist
    /// on, already read off the function.
    pub(crate) fn with(
        func: &Func,
        order: &Order,
        live: &Live,
        env: &Env,
        spilled: &[Reg],
        blocked: &Blocks,
    ) -> Self {
        let points = usize::try_from(order.points()).expect("a point count");
        let mut forced = vec![false; func.vregs()];
        for &reg in spilled {
            forced[index(reg)] = true;
        }
        // One more entry than there are points, so the end of a piece that reaches the last point
        // has somewhere to be taken off at.
        let mut steps: Vec<Vec<i64>> = Vec::new();
        for (number, &forced) in forced.iter().enumerate() {
            let reg = Reg::virtual_reg(u32::try_from(number).expect("a register number"));
            let (Some(area), Some(class)) = (live.area(reg), func.class_of(reg)) else {
                continue;
            };
            if forced {
                continue;
            }
            let class = usize::from(class.number());
            if steps.len() <= class {
                steps.resize_with(class + 1, Vec::new);
            }
            let row = &mut steps[class];
            if row.is_empty() {
                *row = vec![0; points + 1];
            }
            for piece in area.pieces() {
                row[at(piece.start)] += 1;
                row[at(piece.end) + 1] -= 1;
            }
        }
        let wanted = steps
            .into_iter()
            .map(|row| {
                let mut count = 0i64;
                let mut wanted: Vec<u32> = row
                    .iter()
                    .map(|step| {
                        count += step;
                        u32::try_from(count).expect("a count of values")
                    })
                    .collect();
                wanted.truncate(points);
                wanted
            })
            .collect();

        let mut room: Vec<Vec<u32>> = env
            .offered()
            .map(|offered| {
                let count = u32::try_from(offered.len()).expect("a count of registers");
                if count == 0 { Vec::new() } else { vec![count; points] }
            })
            .collect();
        // Sorted by class, register and point, so an instruction that names one register twice at
        // one point is two entries next to each other, and it is one register taken.
        let mut taken: Vec<_> = blocked.taken().collect();
        taken.dedup();
        for (class, reg, point) in taken {
            let Some(row) = room.get_mut(usize::from(class.number())) else { continue };
            if row.is_empty() || !env.order(class).contains(&reg) {
                continue;
            }
            row[at(point)] = row[at(point)].saturating_sub(1);
        }
        Self { wanted, room }
    }

    /// How many values of a class want a register at a point.
    #[must_use]
    pub fn wanted(&self, class: RegClass, point: Point) -> u32 {
        read(&self.wanted, class, point)
    }

    /// How many registers of a class there are at a point.
    #[must_use]
    pub fn room(&self, class: RegClass, point: Point) -> u32 {
        read(&self.room, class, point)
    }

    /// How many of the values of a class live at a point have to be in memory there, at the least.
    #[must_use]
    pub fn excess(&self, class: RegClass, point: Point) -> u32 {
        self.wanted(class, point).saturating_sub(self.room(class, point))
    }

    /// The most values of a class that want a register at any one point.
    #[must_use]
    pub fn peak(&self, class: RegClass) -> u32 {
        row(&self.wanted, class).iter().copied().max().unwrap_or(0)
    }

    /// The points where more values of a class want a register than there are registers, in order.
    pub fn over(&self, class: RegClass) -> impl Iterator<Item = Point> + '_ {
        let wanted = row(&self.wanted, class);
        (0..wanted.len())
            .filter_map(|point| u32::try_from(point).ok())
            .filter(move |&point| self.excess(class, point) > 0)
    }

    /// Takes a value off every point it is live at, which is what sending it to memory does.
    pub(crate) fn lift(&mut self, class: RegClass, area: Area<'_>) {
        let Some(row) = self.wanted.get_mut(usize::from(class.number())) else { return };
        // A slice per piece rather than a lookup per point, so the loop has no bounds to check and
        // the compiler can take one off several counts at once.
        for piece in area.pieces() {
            let end = at(piece.end).saturating_add(1).min(row.len());
            let start = at(piece.start).min(end);
            for count in &mut row[start..end] {
                *count = count.saturating_sub(1);
            }
        }
    }

    /// Puts a value back on every point it is live at, which undoes [`Self::lift`].
    pub(crate) fn lower(&mut self, class: RegClass, area: Area<'_>) {
        let Some(row) = self.wanted.get_mut(usize::from(class.number())) else { return };
        for piece in area.pieces() {
            let end = at(piece.end).saturating_add(1).min(row.len());
            let start = at(piece.start).min(end);
            for count in &mut row[start..end] {
                *count += 1;
            }
        }
    }
}

fn row(table: &[Vec<u32>], class: RegClass) -> &[u32] {
    table.get(usize::from(class.number())).map_or(&[], Vec::as_slice)
}

fn read(table: &[Vec<u32>], class: RegClass, point: Point) -> u32 {
    row(table, class).get(at(point)).copied().unwrap_or(0)
}

fn at(point: Point) -> usize {
    usize::try_from(point).expect("a point")
}

fn index(reg: Reg) -> usize {
    usize::try_from(reg.number().expect("a virtual register")).expect("a register number")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{Opcode, Operand};
    use rucc_target::x86_64::{GPR, RAX, SYSV};

    use super::*;

    fn narrow(count: usize) -> Env {
        Env::new().with(GPR, &SYSV.int_order[..count], &SYSV.int_order[count..count + 1])
    }

    fn of(func: &Func, env: &Env) -> (Order, Pressure) {
        let order = Order::of(func);
        let live = Live::of(func, &order);
        let pressure = Pressure::of(func, &order, &live, env);
        (order, pressure)
    }

    #[test]
    fn every_value_live_at_a_point_is_counted_there() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let values: Vec<Reg> = (0..3).map(|_| func.new_vreg(GPR)).collect();
        let defs: Vec<_> = values
            .iter()
            .map(|&value| func.build(block, opcode).def(value, GPR).finish())
            .collect();
        let reads = func.build(block, opcode);
        let reads = values.iter().fold(reads, |build, &value| build.uses(value, GPR));
        let last = reads.finish();

        let (order, pressure) = of(&func, &narrow(2));
        assert_eq!(pressure.wanted(GPR, order.early(last)), 3);
        assert_eq!(pressure.room(GPR, order.early(last)), 2);
        assert_eq!(pressure.excess(GPR, order.early(last)), 1);
        assert_eq!(pressure.peak(GPR), 3);
        // From where the third is written to where all three are read.
        let over = [order.late(defs[2]), order.early(last)];
        assert_eq!(pressure.over(GPR).collect::<Vec<_>>(), over);
    }

    #[test]
    fn a_register_an_instruction_writes_outright_is_not_room_where_it_writes_it() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let value = func.new_vreg(GPR);
        func.build(block, opcode).def(value, GPR).finish();
        let call =
            func.build(block, opcode).operand(Operand::write(Reg::physical(RAX), GPR)).finish();
        func.build(block, opcode).uses(value, GPR).finish();

        let (order, pressure) = of(&func, &narrow(2));
        assert_eq!(pressure.room(GPR, order.early(call)), 2);
        assert_eq!(pressure.room(GPR, order.late(call)), 1);
        assert_eq!(pressure.wanted(GPR, order.late(call)), 1);
        assert_eq!(pressure.over(GPR).count(), 0);
    }

    #[test]
    fn a_value_sent_to_memory_is_taken_off_every_point_it_was_live_at() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let first = func.new_vreg(GPR);
        let second = func.new_vreg(GPR);
        func.build(block, opcode).def(first, GPR).finish();
        func.build(block, opcode).def(second, GPR).finish();
        let last = func.build(block, opcode).uses(first, GPR).uses(second, GPR).finish();

        let order = Order::of(&func);
        let live = Live::of(&func, &order);
        let mut pressure = Pressure::of(&func, &order, &live, &narrow(1));
        assert_eq!(pressure.excess(GPR, order.early(last)), 1);
        pressure.lift(GPR, live.area(first).expect("live"));
        assert_eq!(pressure.over(GPR).count(), 0);
        assert_eq!(pressure.wanted(GPR, order.early(last)), 1);
    }
}
