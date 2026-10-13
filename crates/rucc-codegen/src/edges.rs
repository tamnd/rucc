//! Moving the copies on a split edge up into the block that branches, so the edge needs no block.
//!
//! [`crate::split`] puts an empty block on every critical edge that carries values, and the
//! allocator writes the moves those values turn into there. When the moves really are wanted on
//! that edge alone the block stays, and laid out it is the moves and a jump. The commonest one is a
//! loop's back edge: the counter is worked out in one register and the top of the loop wants it in
//! another, so a loop over a bitmap on i386 ends like this, where gcc ends it in one jump:
//!
//! ```text
//!   addl $1, %esi                 addl $1, %esi
//!   cmpl $8, %esi                 movl %esi, %ecx
//!   jae  .Ldone                   cmpl $8, %esi
//!   movl %esi, %ecx               jb   .Ltop
//!   jmp  .Ltop
//! ```
//!
//! The moves can go in front of the branch when the other way out has no use for what they
//! overwrite. Then they run on both edges, which is one or two moves on the way out of the loop
//! against a jump on every turn, and the block on the edge is empty and
//! [`crate::layout::forward`] sends the branch past it.
//!
//! # When it is safe
//!
//! Every register a move writes must be dead where the other edge goes, which is what [`live`]
//! works out. The branch and the comparison it is folded with stay at the end of the block, so the
//! moves go in front of the two of them, and neither may read or write a register the moves write
//! or write one they read. Only a copy between two registers, a number or an address put in a
//! register, and the allocator's reload of a slot are moved. Any other load could fault on the
//! path that did not want it and a store could write a slot that path still reads, and a reload
//! is moved only past instructions that leave memory alone. Each of them has to leave the
//! condition state as it found it, and the target is asked rather than believed about that.
//!
//! # When it pays
//!
//! The moves now run on the other edge as well, so the edge they came off has to be taken at
//! least as often as the other one. A back edge is taken far more often than the exit and a two
//! way branch with no profile is an even split, and on an even split a jump costs more than a
//! copy the renamer does for nothing.

use crate::finish::Moves;
use rucc_base::Interner;
use rucc_base::hash::Map;
use rucc_mir as mir;
use rucc_regalloc::assign::Place;
use rucc_target::{BranchInsts, FlagInsts, FrameInsts, MachineInsts, RegClass};

/// Moves the copies out of every block on a split edge that the block above it can take, and
/// gives back how many blocks it emptied.
pub fn lift(
    func: &mut mir::Func,
    branch: &BranchInsts,
    frame: &FrameInsts,
    machine: &MachineInsts,
    flags: &FlagInsts,
    moves: &Moves,
    names: &mut Interner,
) -> usize {
    let cond = mir::Opcode::new(names.join(branch.prefix, branch.cond));
    let kinds: Vec<Kinds> = (0..=u8::MAX)
        .map_while(|number| {
            let class = frame.moves(RegClass::new(number))?;
            let opcode = |names: &mut Interner, name: &'static str| {
                let opcode = mir::Opcode::new(names.join(frame.prefix, name));
                (!(flags.writes)(name)).then_some(opcode)
            };
            Some(Kinds { mov: opcode(names, class.mov), load: opcode(names, class.load) })
        })
        .collect();
    let mut preds: Map<mir::Block, Vec<mir::Block>> = Map::default();
    for block in func.blocks() {
        for arm in &func[block].succs {
            preds.entry(arm.block).or_default().push(block);
        }
    }
    let named = |func: &mir::Func, block: mir::Block| {
        func.block_name(block).is_some() || func.landings.iter().any(|&(_, pad)| pad == block)
    };

    // Each edge block that could go, with the block above it and the other way out of that one.
    let mut found: Vec<(mir::Block, mir::Block, mir::Block)> = Vec::new();
    for edge in func.blocks() {
        let data = &func[edge];
        let ([arm], Some([above])) = (&data.succs[..], preds.get(&edge).map(Vec::as_slice)) else {
            continue;
        };
        if !arm.args.is_empty() || !data.params.is_empty() || data.dead_end || named(func, edge) {
            continue;
        }
        let copies = func
            .insts(edge)
            .all(|inst| copy(func, inst, &kinds, (moves, machine, flags), names).is_some());
        if func.terminator(edge).is_none() || !copies || Some(edge) == func.entry() {
            continue;
        }
        let Some(last) = func.terminator(*above) else { continue };
        let [first, second] = &func[*above].succs[..] else { continue };
        if func[last].opcode != cond || first.block == second.block {
            continue;
        }
        let (taken, other) = if first.block == edge { (first, second) } else { (second, first) };
        if taken.weight.raw() < other.weight.raw() {
            continue;
        }
        found.push((edge, *above, other.block));
    }
    if found.is_empty() {
        return 0;
    }

    let live = live(func, machine, names);
    let mut emptied = 0;
    for (edge, above, other) in found {
        let copies: Option<Vec<(mir::Inst, Copy)>> = func
            .insts(edge)
            .map(|inst| Some((inst, copy(func, inst, &kinds, (moves, machine, flags), names)?)))
            .collect();
        let (Some(copies), Some(last)) = (copies, func.terminator(above)) else { continue };
        if copies.iter().any(|(_, copy)| live[other.index()].contains(&copy.to)) {
            continue;
        }
        // The branch, and the comparison in front of it when that is what it reads, stay last.
        let mut at = last;
        if let Some(before) = previous(func, last) {
            let reads = keys(func, last, false);
            if keys(func, before, true).iter().any(|key| reads.contains(key)) {
                at = before;
            }
        }
        let tail: Vec<mir::Inst> =
            std::iter::successors(Some(at), |&inst| func.next_inst(inst)).collect();
        let loads = copies.iter().any(|(_, copy)| copy.load);
        let clash = tail.iter().any(|&inst| {
            let name = names.resolve(func[inst].opcode.name());
            let (read, written) = (keys(func, inst, false), keys(func, inst, true));
            opaque(func, inst, machine, names)
                || (loads && machine.touches_mem(name))
                || copies.iter().any(|(_, copy)| {
                    read.contains(&copy.to)
                        || written.contains(&copy.to)
                        || copy.from.is_some_and(|from| written.contains(&from))
                })
        });
        if clash || copies.iter().any(|&(inst, _)| func.cfi_after(inst).next().is_some()) {
            continue;
        }
        for (inst, _) in copies {
            func.remove_inst(inst);
            func.insert_before(at, inst);
        }
        emptied += 1;
    }
    emptied
}

/// A physical register, by its class's number and its number in the class.
type Key = (u8, u8);

fn key(operand: &mir::Operand) -> Option<Key> {
    operand.reg.phys().map(|reg| (operand.class.number(), reg.number()))
}

/// The move opcodes of one register class that leave the condition state alone.
struct Kinds {
    /// From one register into another.
    mov: Option<mir::Opcode>,
    /// From a slot of the frame into a register.
    load: Option<mir::Opcode>,
}

/// A move that may run on the edge it was not written for.
struct Copy {
    /// The register it writes.
    to: Key,
    /// The register it reads, if it reads one.
    from: Option<Key>,
    /// Whether it reads the frame.
    load: bool,
}

/// What a move reads and writes, or `None` for anything that is not one that may run where it was
/// not asked for: a copy of one register into another of its class, a number or the address of a
/// symbol written into a register, or the allocator's reload of a value it had put in the frame.
///
/// A reload reads a slot and nothing else, which no path can fault on and which holds the value it
/// held on the path that wanted it until something stores to it. Any other load could fault, and
/// a store could write a slot the other path still reads.
fn copy(
    func: &mir::Func,
    inst: mir::Inst,
    kinds: &[Kinds],
    (moves, machine, flags): (&Moves, &MachineInsts, &FlagInsts),
    names: &Interner,
) -> Option<Copy> {
    let data = &func[inst];
    match &func[data.operands] {
        [to] if data.mem.is_none() && (data.imm.is_some() || data.symbol.is_some()) => {
            let name = names.resolve(data.opcode.name());
            let bare = machine.bare(name);
            let plain = machine.has(name) && !machine.calls(name) && !machine.touches_mem(name);
            (to.role.is_def() && plain && !(flags.writes)(bare))
                .then(|| Some(Copy { to: key(to)?, from: None, load: false }))
                .flatten()
        }
        // The register the slot is found from is the one operand after the answer.
        [to, base] if data.mem.is_some() && data.imm.is_none() && data.symbol.is_none() => {
            let load = kinds.get(usize::from(to.class.number()))?.load?;
            let edit = moves.at(inst)?;
            let reload = matches!((edit.mov.from, edit.mov.to), (Place::Slot(_), Place::Reg(_)));
            if data.opcode != load || !to.role.is_def() || base.role.is_def() || !reload {
                return None;
            }
            Some(Copy { to: key(to)?, from: Some(key(base)?), load: true })
        }
        [to, from] if data.mem.is_none() && data.imm.is_none() && data.symbol.is_none() => {
            let mov = kinds.get(usize::from(to.class.number()))?.mov?;
            if data.opcode != mov
                || !to.role.is_def()
                || from.role.is_def()
                || to.class != from.class
            {
                return None;
            }
            let (to, from) = (key(to)?, key(from)?);
            (to != from).then_some(Copy { to, from: Some(from), load: false })
        }
        _ => None,
    }
}

/// The registers an instruction writes, or the ones it reads.
///
/// A two address form reads the register it writes, and says so by tying the answer to a source.
/// That counts as a read here whether or not the source still names the same register.
fn keys(func: &mir::Func, inst: mir::Inst, written: bool) -> Vec<Key> {
    let operands = &func[func[inst].operands];
    let wanted = |operand: &&mir::Operand| {
        if written {
            operand.role.is_def()
        } else {
            !operand.role.is_def() || matches!(operand.constraint, mir::Constraint::Reuse(_))
        }
    };
    operands.iter().filter(wanted).filter_map(key).collect()
}

/// Whether an instruction may read or write registers its operands do not name.
fn opaque(func: &mir::Func, inst: mir::Inst, machine: &MachineInsts, names: &Interner) -> bool {
    let name = names.resolve(func[inst].opcode.name());
    machine.calls(name) || !machine.has(name)
}

/// The instruction in front of this one in its block.
fn previous(func: &mir::Func, inst: mir::Inst) -> Option<mir::Inst> {
    let block = func.block_of(inst)?;
    let mut before = None;
    for at in func.insts(block) {
        if at == inst {
            return before;
        }
        before = Some(at);
    }
    None
}

/// What each block may read before writing it, by block.
///
/// A register is in the set when some way on from the start of the block reads it before anything
/// writes it. An instruction this target does not describe may read registers its operands do not
/// name, so it reads all of them, which `all` stands for. What a block hands its successors is
/// read at its end. A block that leaves the function reads what its last instructions name and
/// nothing else, since a return names what it returns and the epilogue puts back what a call keeps
/// out of the frame.
fn live(func: &mir::Func, machine: &MachineInsts, names: &Interner) -> Vec<Set> {
    let blocks: Vec<mir::Block> = func.blocks().collect();
    let mut gen_: Vec<Set> = vec![Set::default(); func.block_count()];
    let mut kill: Vec<Set> = vec![Set::default(); func.block_count()];
    for &block in &blocks {
        let (gen_, kill) = (&mut gen_[block.index()], &mut kill[block.index()]);
        for inst in func.insts(block) {
            if gen_.all {
                break;
            }
            // A call names the registers it passes and the ones it clobbers, and leaves the rest
            // as it found them. One that ends a block that goes nowhere does not come back, and
            // one in tail position is still a call with the return behind it here.
            if !machine.has(names.resolve(func[inst].opcode.name())) {
                gen_.all = true;
                break;
            }
            for key in keys(func, inst, false) {
                if !kill.contains(&key) {
                    gen_.insert(key);
                }
            }
            for key in keys(func, inst, true) {
                kill.insert(key);
            }
        }
        for arm in &func[block].succs {
            for reg in &arm.args {
                if let Some(phys) = reg.phys() {
                    // The class of an argument is the class of the parameter it lands in, which
                    // the edge does not say, so every class's register of that number is taken.
                    gen_.numbers.push(phys.number());
                }
            }
        }
    }
    let mut live: Vec<Set> = gen_.clone();
    let mut changed = true;
    while changed {
        changed = false;
        for &block in blocks.iter().rev() {
            let mut out = Set::default();
            for arm in &func[block].succs {
                out.union(&live[arm.block.index()]);
            }
            out.subtract(&kill[block.index()]);
            let at = &mut live[block.index()];
            if at.absorb(&out) {
                changed = true;
            }
        }
    }
    live
}

/// A set of registers, with a flag for every register at once.
#[derive(Debug, Clone, Default)]
struct Set {
    /// Every register, which is what an instruction that may read anything reads.
    all: bool,
    /// Registers of any class with this number, which is what an edge argument is.
    numbers: Vec<u8>,
    /// The registers named one at a time.
    keys: Vec<Key>,
}

impl Set {
    fn contains(&self, key: &Key) -> bool {
        self.all || self.numbers.contains(&key.1) || self.keys.contains(key)
    }

    fn insert(&mut self, key: Key) {
        if !self.keys.contains(&key) {
            self.keys.push(key);
        }
    }

    fn union(&mut self, other: &Self) {
        self.all |= other.all;
        for &number in &other.numbers {
            if !self.numbers.contains(&number) {
                self.numbers.push(number);
            }
        }
        for &key in &other.keys {
            self.insert(key);
        }
    }

    /// Takes out what a block writes before reading. An edge argument or an `all` that came in
    /// from below stays, since a block that writes one register of a number has not written the
    /// others and the set does not say which class was meant.
    fn subtract(&mut self, kill: &Self) {
        self.keys.retain(|key| !kill.keys.contains(key));
    }

    /// Adds what it does not have yet, and says whether that was anything.
    fn absorb(&mut self, other: &Self) -> bool {
        let before = (self.all, self.numbers.len(), self.keys.len());
        self.union(other);
        before != (self.all, self.numbers.len(), self.keys.len())
    }
}

#[cfg(test)]
mod tests {
    use rucc_mir::{BlockCall, Mem, Opcode, Operand, Reg, Weight};
    use rucc_regalloc::moves::Move;
    use rucc_regalloc::rewrite::{At, Edit};
    use rucc_target::PhysReg;
    use rucc_target::x86_64::{BRANCH, FLAGS, FRAME, GPR, MACHINE, RAX, RCX, RDX, RSI};

    use super::*;

    /// The opcode of that name on this target.
    fn op(names: &mut Interner, name: &str) -> Opcode {
        Opcode::new(names.intern(&format!("{}{name}", FRAME.prefix)))
    }

    /// A loop of one block that compares two registers and goes round again by way of the block
    /// on its back edge, which is empty for the test to fill. Gives back the function, the loop,
    /// the back edge block and the exit, which returns.
    fn looped(names: &mut Interner, exit_reads: Option<PhysReg>) -> (mir::Func, [mir::Block; 3]) {
        let mut func = mir::Func::new(names.intern("f"));
        let made = [func.create_block(), func.create_block(), func.create_block()];
        func.build(made[0], op(names, "cmp_set_l_32"))
            .def(Reg::physical(RDX), GPR)
            .operand(Operand::read(Reg::physical(RAX), GPR))
            .operand(Operand::read(Reg::physical(RCX), GPR))
            .finish();
        func.build(made[0], op(names, "br_cond_8"))
            .operand(Operand::read(Reg::physical(RDX), GPR))
            .finish();
        *func.succs_mut(made[0]) = vec![
            BlockCall::to(made[1]).taken(Weight::parts(9)),
            BlockCall::to(made[2]).taken(Weight::parts(1)),
        ];
        *func.succs_mut(made[1]) = vec![BlockCall::to(made[0])];
        let mut ret = func.build(made[2], op(names, "ret"));
        if let Some(reg) = exit_reads {
            ret = ret.operand(Operand::read(Reg::physical(reg), GPR));
        }
        ret.finish();
        (func, made)
    }

    /// The pass, on the one target these tests are written against.
    fn lifted(func: &mut mir::Func, moves: &Moves, names: &mut Interner) -> usize {
        lift(func, &BRANCH, &FRAME, &MACHINE, &FLAGS, moves, names)
    }

    /// The names of what is in a block, in order.
    fn opcodes(func: &mir::Func, block: mir::Block, names: &Interner) -> Vec<String> {
        func.insts(block).map(|inst| names.resolve(func[inst].opcode.name()).to_string()).collect()
    }

    #[test]
    fn a_copy_on_a_back_edge_goes_in_front_of_the_comparison() {
        let mut names = Interner::new();
        let (mut func, made) = looped(&mut names, None);
        func.build(made[1], op(&mut names, "mov_rr_64"))
            .def(Reg::physical(RSI), GPR)
            .uses(Reg::physical(RAX), GPR)
            .finish();

        assert_eq!(lifted(&mut func, &Moves::default(), &mut names), 1);
        assert_eq!(func.insts(made[1]).count(), 0);
        assert_eq!(
            opcodes(&func, made[0], &names),
            ["x64.mov_rr_64", "x64.cmp_set_l_32", "x64.br_cond_8"]
        );
    }

    #[test]
    fn a_copy_the_exit_reads_over_stays_on_its_edge() {
        let mut names = Interner::new();
        let (mut func, made) = looped(&mut names, Some(RSI));
        func.build(made[1], op(&mut names, "mov_rr_64"))
            .def(Reg::physical(RSI), GPR)
            .uses(Reg::physical(RAX), GPR)
            .finish();

        assert_eq!(lifted(&mut func, &Moves::default(), &mut names), 0);
        assert_eq!(func.insts(made[1]).count(), 1);
    }

    #[test]
    fn a_copy_on_the_rarer_edge_stays_on_it() {
        let mut names = Interner::new();
        let (mut func, made) = looped(&mut names, None);
        func.build(made[1], op(&mut names, "mov_rr_64"))
            .def(Reg::physical(RSI), GPR)
            .uses(Reg::physical(RAX), GPR)
            .finish();
        for arm in func.succs_mut(made[0]) {
            arm.weight = if arm.block == made[1] { Weight::parts(1) } else { Weight::parts(9) };
        }

        assert_eq!(lifted(&mut func, &Moves::default(), &mut names), 0);
    }

    #[test]
    fn a_number_on_a_back_edge_goes_in_front_of_the_comparison() {
        let mut names = Interner::new();
        let (mut func, made) = looped(&mut names, None);
        func.build(made[1], op(&mut names, "mov_ri_64"))
            .def(Reg::physical(RSI), GPR)
            .imm(5)
            .finish();

        assert_eq!(lifted(&mut func, &Moves::default(), &mut names), 1);
        assert_eq!(func.insts(made[1]).count(), 0);
    }

    /// A load from the frame is moved when the allocator says it is the reload of a slot it
    /// spilled to, and not otherwise, since any other load could be of memory the exit never
    /// wanted read.
    #[test]
    fn a_reload_on_a_back_edge_goes_and_a_load_that_is_not_a_reload_stays() {
        for reload in [true, false] {
            let mut names = Interner::new();
            let (mut func, made) = looped(&mut names, None);
            let base = Operand::read(Reg::physical(RCX), GPR);
            let load = func
                .build(made[1], op(&mut names, "mov_rm_64"))
                .def(Reg::physical(RSI), GPR)
                .mem(Mem::at(base).plus(8))
                .finish();
            let mut moves = Moves::default();
            if reload {
                let mov = Move::new(Place::Reg(RSI), Place::Slot(0));
                moves.record(load, Edit { at: At::StartOf(made[1]), mov, class: GPR });
            }

            assert_eq!(lifted(&mut func, &moves, &mut names), usize::from(reload));
        }
    }
}
