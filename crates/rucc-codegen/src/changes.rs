//! Proposing a set of machine level changes, asking whether the target takes them, and either
//! committing the set or dropping it.
//!
//! Design: `spec/optimizer/37-machine-level-optimization.md` sections 37.2 and 37.3.
//!
//! Section 37.3 quotes `gcc/combine.cc` on what a machine level rewrite is: substitute the earlier
//! instruction into the later one, ask the machine description whether the result is an instruction
//! this target has, install it if it is and put everything back if it is not. Section 37.2 reads
//! `gcc/rtl-ssa/changes.cc` and says the same thing about the arrangement rather than the rewrite,
//! which is that the propose, validate and commit belongs to one named component rather than to
//! each pass in its own words. This is that component.
//!
//! # Nothing is written until the whole set is taken
//!
//! A proposal holds what the instruction would become by value: the operands in a vector of their
//! own, the addressing mode as an [`mir::Amode`] rather than as a reference into the function's
//! arena, the immediate as a number. So abandoning a set is dropping it and there is no undo to
//! get wrong. That is the difference between this and GCC's, which edits the RTL in place and
//! keeps a list of what to put back, and it is available here because machine IR keeps an
//! instruction's parts in arenas the proposal can stay out of until the last moment.
//!
//! # Why a set rather than an instruction
//!
//! Because the interesting rewrites are all of them or none. Folding an address into the three
//! instructions that read it is worth doing when the address computation goes, and folding it into
//! two of the three is worth nothing: the computation stays for the third, the address is worked
//! out twice, and the registers it reads are live across all three. `crate::fold` had that rule
//! written into it by hand and so would every pass after it.
//!
//! # What the target is asked
//!
//! [`MachineInsts`] is the whole of it, and every question in it is answered out of the same
//! description the allocator and the encoder read. An opcode this machine does not have, an
//! operand vector that is not the shape the opcode's form says, an immediate on an instruction
//! that carries none, an addressing mode on one that has none, a scale this machine cannot write:
//! each of those is a refusal, and a refusal is the whole set's.
//!
//! What is checked beyond the target's description is the part that is about the function rather
//! than about the machine. An instruction may be named once in a set, it has to still be in the
//! function, and taking an instruction out is refused while anything still reads what it wrote.
//! That last one is what the set is for, so [`Changes`] is the thing that knows it rather than
//! each pass.

use std::collections::HashMap;

use rucc_base::{Interner, Symbol};
use rucc_mir::{self as mir, Role};
use rucc_target::MachineInsts;

/// What an instruction would become.
///
/// Every part is held by value rather than as a reference into the function, which is what lets a
/// proposal be dropped rather than undone. [`Changes::commit`] is what puts the parts in the
/// arenas, and until it runs the function does not know this exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Which instruction it becomes.
    pub opcode: mir::Opcode,
    /// Its operands, the ones it writes before the ones it reads, with the registers its
    /// addressing mode names last. The order is the one [`mir::InstBuilder`] keeps and the
    /// printer, the parser and the allocator all read.
    pub operands: Vec<mir::Operand>,
    /// Its immediate, if the instruction carries one.
    pub imm: Option<i64>,
    /// Its addressing mode, if the instruction has one. The base and the index are positions in
    /// `operands`.
    pub amode: Option<mir::Amode>,
    /// The symbol it names, which is the callee of a direct call.
    pub symbol: Option<Symbol>,
}

impl Plan {
    /// The instruction as it stands, which is where a rewrite starts from.
    #[must_use]
    pub fn of(func: &mir::Func, inst: mir::Inst) -> Self {
        let data = &func[inst];
        Self {
            opcode: data.opcode,
            operands: func[data.operands].to_vec(),
            imm: data.imm.map(|at| func[at].0),
            amode: data.mem.map(|at| func[at]),
            symbol: data.symbol,
        }
    }

    /// The registers it reads, which is what a removal has to count.
    fn reads(&self) -> impl Iterator<Item = mir::Reg> + use<'_> {
        self.operands.iter().filter(|operand| operand.role == Role::Use).map(|operand| operand.reg)
    }
}

/// Why the target or the function would not have a set.
///
/// One instruction's refusal rather than the set's, because a pass that wants to know what it did
/// wrong wants to know where, and because the tests below are clearer for it. Every one of them
/// turns down the set it is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The set names that instruction twice, so what it becomes depends on which half wins.
    Twice(mir::Inst),
    /// That instruction is not in the function, which is a set built against a function something
    /// else has changed since.
    Gone(mir::Inst),
    /// This target has no instruction of that name.
    Unknown(mir::Inst),
    /// The operand vector is not the shape the opcode's description says it is.
    Operands(mir::Inst),
    /// An immediate on an instruction that carries none, or none on one that does.
    Imm(mir::Inst),
    /// An addressing mode on an instruction that has none, or none on one that does.
    Mem(mir::Inst),
    /// An index multiplied by something this machine cannot write.
    Scale(mir::Inst),
    /// Taking that instruction out would leave something reading a register it wrote.
    Read(mir::Inst),
}

/// How many times each register is read, kept across the commits of one pass.
///
/// A removal has to know whether anything still reads what the instruction wrote, and asking the
/// function that question once per commit is the length of the function once per commit. So it is
/// asked once and the answer is carried, which each commit brings up to date with what it did.
#[derive(Debug, Clone, Default)]
pub struct Reads {
    counts: HashMap<mir::Reg, usize>,
}

impl Reads {
    /// Every read in the function, counting the arguments an edge carries as reads, which they
    /// are.
    #[must_use]
    pub fn of(func: &mir::Func) -> Self {
        let mut counts: HashMap<mir::Reg, usize> = HashMap::new();
        for block in func.blocks() {
            for inst in func.insts(block) {
                for operand in &func[func[inst].operands] {
                    if operand.role == Role::Use {
                        *counts.entry(operand.reg).or_insert(0) += 1;
                    }
                }
            }
            for call in &func[block].succs {
                for &arg in &call.args {
                    *counts.entry(arg).or_insert(0) += 1;
                }
            }
        }
        Self { counts }
    }

    /// How many reads of that register there are.
    #[must_use]
    pub fn count(&self, reg: mir::Reg) -> usize {
        self.counts.get(&reg).copied().unwrap_or(0)
    }

    /// Records one read more.
    fn gained(&mut self, reg: mir::Reg) {
        *self.counts.entry(reg).or_insert(0) += 1;
    }

    /// Records one read fewer.
    fn lost(&mut self, reg: mir::Reg) {
        if let Some(count) = self.counts.get_mut(&reg) {
            *count = count.saturating_sub(1);
        }
    }
}

/// What one instruction in a set would have done to it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum What {
    /// Becomes that.
    Rewrite(Plan),
    /// Goes.
    Remove,
}

/// A set of changes to one function, proposed together and taken together.
///
/// The order proposals are added in is the order they are applied in, which matters only for the
/// reading of a commit that both rewrites and removes: nothing here depends on it, since a plan
/// says what an instruction becomes rather than what to do to what it is.
#[derive(Debug, Clone, Default)]
pub struct Changes {
    changes: Vec<(mir::Inst, What)>,
}

impl Changes {
    /// A set with nothing in it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many instructions the set is about.
    #[must_use]
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// Whether the set is about nothing, which commits and changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Proposes that the instruction become that.
    pub fn rewrite(&mut self, inst: mir::Inst, plan: Plan) {
        self.changes.push((inst, What::Rewrite(plan)));
    }

    /// Proposes that the instruction go.
    pub fn remove(&mut self, inst: mir::Inst) {
        self.changes.push((inst, What::Remove));
    }

    /// Why the set would not be taken, or `None` if it would.
    ///
    /// Asked of the function as it stands and of the target's description of itself. Nothing here
    /// changes anything, so a pass may ask, decide the answer is not worth having, and drop the
    /// set.
    #[must_use]
    pub fn refused(
        &self,
        func: &mir::Func,
        reads: &Reads,
        names: &Interner,
        machine: &MachineInsts,
    ) -> Option<Refusal> {
        for (at, &(inst, _)) in self.changes.iter().enumerate() {
            if self.changes[..at].iter().any(|&(other, _)| other == inst) {
                return Some(Refusal::Twice(inst));
            }
            if func.block_of(inst).is_none() {
                return Some(Refusal::Gone(inst));
            }
        }
        for &(inst, ref what) in &self.changes {
            match what {
                What::Rewrite(plan) => {
                    if let Some(refusal) = shaped(inst, plan, names, machine) {
                        return Some(refusal);
                    }
                }
                What::Remove => {
                    if self.read_after(func, reads, inst) {
                        return Some(Refusal::Read(inst));
                    }
                }
            }
        }
        None
    }

    /// Takes the set if the target and the function will have it, and gives back how many
    /// instructions it touched.
    ///
    /// # Errors
    ///
    /// The first [`Refusal`] the set earns, with nothing written. A refused set leaves the
    /// function exactly as it was.
    pub fn commit(
        self,
        func: &mut mir::Func,
        reads: &mut Reads,
        names: &Interner,
        machine: &MachineInsts,
    ) -> Result<usize, Refusal> {
        if let Some(refusal) = self.refused(func, reads, names, machine) {
            return Err(refusal);
        }
        let touched = self.changes.len();
        for (inst, what) in self.changes {
            for operand in &func[func[inst].operands] {
                if operand.role == Role::Use {
                    reads.lost(operand.reg);
                }
            }
            match what {
                What::Rewrite(plan) => {
                    for reg in plan.reads() {
                        reads.gained(reg);
                    }
                    let operands = func.push_operands(&plan.operands);
                    let imm = plan.imm.map(|value| func.add_imm(value));
                    let mem = plan.amode.map(|amode| func.add_amode(amode));
                    let data = &mut func[inst];
                    data.opcode = plan.opcode;
                    data.operands = operands;
                    data.imm = imm;
                    data.mem = mem;
                    data.symbol = plan.symbol;
                }
                What::Remove => func.remove_inst(inst),
            }
        }
        Ok(touched)
    }

    /// Whether anything the set leaves behind reads a register that instruction writes.
    ///
    /// The counts are of the function as it stands, so what the set is about has to be taken off
    /// them: a read in an instruction this set removes is a read that is going, and a read in one
    /// it rewrites is going if the plan does not have it back. What is left after that is the
    /// reads nothing in this set is doing anything about, and one of those is enough to keep the
    /// instruction where it is.
    fn read_after(&self, func: &mir::Func, reads: &Reads, inst: mir::Inst) -> bool {
        func[func[inst].operands].iter().filter(|operand| operand.role.is_def()).any(|operand| {
            let mut left = reads.count(operand.reg);
            for &(other, ref what) in &self.changes {
                for read in func[func[other].operands].iter().filter(|o| o.role == Role::Use) {
                    if read.reg == operand.reg {
                        left = left.saturating_sub(1);
                    }
                }
                if let What::Rewrite(plan) = what {
                    left += plan.reads().filter(|&reg| reg == operand.reg).count();
                }
            }
            left != 0
        })
    }
}

/// Why the target would not have that instruction, or `None` if it would.
///
/// The description says which operands the instruction has, which is the ones it writes and then
/// the ones it reads, and it stops there: the registers an addressing mode names are operands the
/// addressing mode knows the positions of, so what is checked about those is that they are reads,
/// that the mode points at them, and that there are no others.
///
/// The constraint is checked along with the class and the role, because it is part of what the
/// instruction is rather than part of what a pass may choose. An `add` on this machine writes its
/// answer into the register it read, the description says so with a reuse constraint, and a
/// proposal that leaves the constraint off is asking for an instruction this machine has no
/// encoding for. Nothing downstream would catch it either: the allocator gives an operand whatever
/// its constraint asks for, so a missing constraint is a register pair that is allocated apart and
/// then printed as one instruction.
fn shaped(
    inst: mir::Inst,
    plan: &Plan,
    names: &Interner,
    machine: &MachineInsts,
) -> Option<Refusal> {
    let name = names.resolve(plan.opcode.name());
    let bare = machine.bare(name);
    let Some(desc) = (machine.operands)(bare) else { return Some(Refusal::Unknown(inst)) };
    if plan.operands.len() < desc.len() {
        return Some(Refusal::Operands(inst));
    }
    let (described, addressed) = plan.operands.split_at(desc.len());
    for (operand, want) in described.iter().zip(desc) {
        let shape = (operand.class, operand.role, operand.constraint);
        if shape != (want.class, want.role, want.constraint) {
            return Some(Refusal::Operands(inst));
        }
    }
    if plan.imm.is_some() != (machine.takes_imm)(bare) {
        return Some(Refusal::Imm(inst));
    }
    let Some(amode) = plan.amode else {
        return ((machine.takes_mem)(bare) || !addressed.is_empty()).then_some(Refusal::Mem(inst));
    };
    if !(machine.takes_mem)(bare) {
        return Some(Refusal::Mem(inst));
    }
    let named = [amode.base, amode.index].into_iter().flatten();
    let mut wanted = 0;
    for at in named {
        let Some(operand) = plan.operands.get(usize::from(at)) else {
            return Some(Refusal::Operands(inst));
        };
        if usize::from(at) < desc.len() || operand.role != Role::Use {
            return Some(Refusal::Operands(inst));
        }
        wanted += 1;
    }
    if addressed.len() != wanted {
        return Some(Refusal::Operands(inst));
    }
    let scaled = if amode.index.is_some() { amode.scale } else { 1 };
    if !machine.scales(scaled) || (amode.index.is_none() && amode.scale != 1) {
        return Some(Refusal::Scale(inst));
    }
    None
}

#[cfg(test)]
mod tests {
    use rucc_mir::Constraint;
    use rucc_target::x86_64::{GPR, MACHINE, XMM};

    use super::*;

    /// A function with one block, and the names it was built with.
    fn empty() -> (Interner, mir::Func, mir::Block) {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));
        let block = func.create_block();
        (names, func, block)
    }

    /// The opcode of that name on this target.
    fn op(names: &mut Interner, name: &str) -> mir::Opcode {
        mir::Opcode::new(names.intern(&format!("{}{name}", MACHINE.prefix)))
    }

    /// Whether the target and the function would have the set.
    fn refused(func: &mir::Func, names: &Interner, set: &Changes) -> Option<Refusal> {
        set.refused(func, &Reads::of(func), names, &MACHINE)
    }

    /// What every instruction in a block came to, as opcodes.
    fn shape(func: &mir::Func, names: &Interner, block: mir::Block) -> Vec<String> {
        func.insts(block).map(|inst| names.resolve(func[inst].opcode.name()).to_owned()).collect()
    }

    /// A copy, which is the smallest instruction with an operand of each kind.
    fn copy(
        func: &mut mir::Func,
        names: &mut Interner,
        block: mir::Block,
    ) -> (mir::Inst, mir::Reg) {
        let from = func.new_vreg(GPR);
        let into = func.new_vreg(GPR);
        let mov = op(names, "mov_rr_64");
        (func.build(block, mov).def(into, GPR).uses(from, GPR).finish(), into)
    }

    /// The shape the whole thing is for: an instruction becomes another the target has, and the
    /// function says so afterwards.
    #[test]
    fn a_rewrite_the_target_has_is_taken() {
        let (mut names, mut func, block) = empty();
        let (mov, into) = copy(&mut func, &mut names, block);
        let base = func.new_vreg(GPR);
        let load = op(&mut names, "mov_rm_64");
        let mut set = Changes::new();
        set.rewrite(
            mov,
            Plan {
                opcode: load,
                operands: vec![mir::Operand::write(into, GPR), mir::Operand::read(base, GPR)],
                imm: None,
                amode: Some(mir::Amode {
                    base: Some(1),
                    index: None,
                    scale: 1,
                    disp: 8,
                    symbol: None,
                    block: None,
                    reach: mir::Reach::Itself,
                    segment: None,
                }),
                symbol: None,
            },
        );

        let mut reads = Reads::of(&func);
        assert_eq!(set.commit(&mut func, &mut reads, &names, &MACHINE), Ok(1));

        assert_eq!(shape(&func, &names, block), ["x64.mov_rm_64"]);
        assert_eq!(func[func[mov].mem.expect("the load has an address")].disp, 8);
        assert_eq!(reads.count(base), 1, "the address register is read now");
        assert_eq!(reads.count(into), 0, "the register the copy read is read by nothing");
    }

    /// An opcode nobody described. The proposal is the pass's mistake rather than the target's, and
    /// this is where it stops.
    #[test]
    fn an_opcode_this_target_does_not_have_is_refused() {
        let (mut names, mut func, block) = empty();
        let (mov, into) = copy(&mut func, &mut names, block);
        let made_up = op(&mut names, "mov_rr_65");
        let mut set = Changes::new();
        set.rewrite(
            mov,
            Plan {
                opcode: made_up,
                operands: vec![mir::Operand::write(into, GPR), mir::Operand::read(into, GPR)],
                imm: None,
                amode: None,
                symbol: None,
            },
        );

        assert_eq!(refused(&func, &names, &set), Some(Refusal::Unknown(mov)));
        assert_eq!(shape(&func, &names, block), ["x64.mov_rr_64"], "the function was written to");
    }

    /// An operand of the wrong class. The target's description of a copy between general registers
    /// says both are general registers, and a proposal that reads a vector register is a different
    /// instruction with the same name.
    #[test]
    fn an_operand_of_the_wrong_class_is_refused() {
        let (mut names, mut func, block) = empty();
        let (mov, into) = copy(&mut func, &mut names, block);
        let float = func.new_vreg(XMM);
        let same = func[mov].opcode;
        let mut set = Changes::new();
        set.rewrite(
            mov,
            Plan {
                opcode: same,
                operands: vec![mir::Operand::write(into, GPR), mir::Operand::read(float, XMM)],
                imm: None,
                amode: None,
                symbol: None,
            },
        );

        assert_eq!(refused(&func, &names, &set), Some(Refusal::Operands(mov)));
    }

    /// The constraint is part of the instruction. An `add` writes its answer where it read its
    /// first operand, and a proposal that leaves that off is asking for an encoding this machine
    /// does not have.
    #[test]
    fn an_add_whose_answer_is_not_tied_to_its_source_is_refused() {
        let (mut names, mut func, block) = empty();
        let (mov, into) = copy(&mut func, &mut names, block);
        let other = func.new_vreg(GPR);
        let add = op(&mut names, "add_rr_64");
        let loose = vec![
            mir::Operand::write(into, GPR),
            mir::Operand::read(into, GPR),
            mir::Operand::read(other, GPR),
        ];
        let mut set = Changes::new();
        set.rewrite(
            mov,
            Plan { opcode: add, operands: loose.clone(), imm: None, amode: None, symbol: None },
        );
        assert_eq!(refused(&func, &names, &set), Some(Refusal::Operands(mov)));

        let mut tied = loose;
        tied[0] = tied[0].with(Constraint::Reuse(1));
        let mut set = Changes::new();
        set.rewrite(
            mov,
            Plan { opcode: add, operands: tied, imm: None, amode: None, symbol: None },
        );
        assert_eq!(refused(&func, &names, &set), None, "the same instruction written properly");
    }

    /// An immediate belongs to the instructions that carry one, and to no others. Both ways round,
    /// because a pass that drops an immediate is as wrong as one that invents it.
    #[test]
    fn an_immediate_has_to_be_there_exactly_when_the_instruction_carries_one() {
        let (mut names, mut func, block) = empty();
        let (mov, into) = copy(&mut func, &mut names, block);
        let same = func[mov].opcode;
        let add = op(&mut names, "add_ri_64");
        let mut set = Changes::new();
        set.rewrite(
            mov,
            Plan {
                opcode: same,
                operands: vec![mir::Operand::write(into, GPR), mir::Operand::read(into, GPR)],
                imm: Some(7),
                amode: None,
                symbol: None,
            },
        );
        assert_eq!(refused(&func, &names, &set), Some(Refusal::Imm(mov)), "a copy of seven");

        let tied = vec![
            mir::Operand::write(into, GPR).with(Constraint::Reuse(1)),
            mir::Operand::read(into, GPR),
        ];
        let mut set = Changes::new();
        set.rewrite(
            mov,
            Plan { opcode: add, operands: tied.clone(), imm: None, amode: None, symbol: None },
        );
        assert_eq!(refused(&func, &names, &set), Some(Refusal::Imm(mov)), "an add of nothing");

        let mut set = Changes::new();
        set.rewrite(
            mov,
            Plan { opcode: add, operands: tied, imm: Some(7), amode: None, symbol: None },
        );
        assert_eq!(refused(&func, &names, &set), None);
    }

    /// An address belongs to the instructions that have one. A copy with an address is a load and
    /// has a different name, which is exactly the mistake a pass folding addresses can make.
    #[test]
    fn an_address_on_an_instruction_that_has_none_is_refused() {
        let (mut names, mut func, block) = empty();
        let (mov, into) = copy(&mut func, &mut names, block);
        let base = func.new_vreg(GPR);
        let same = func[mov].opcode;
        let mut set = Changes::new();
        set.rewrite(
            mov,
            Plan {
                opcode: same,
                operands: vec![mir::Operand::write(into, GPR), mir::Operand::read(base, GPR)],
                imm: None,
                amode: Some(mir::Amode {
                    base: Some(1),
                    index: None,
                    scale: 1,
                    disp: 0,
                    symbol: None,
                    block: None,
                    reach: mir::Reach::Itself,
                    segment: None,
                }),
                symbol: None,
            },
        );

        assert_eq!(refused(&func, &names, &set), Some(Refusal::Mem(mov)));
    }

    /// A load with no address at all, which is the same mistake the other way round.
    #[test]
    fn an_instruction_that_wants_an_address_and_has_none_is_refused() {
        let (mut names, mut func, block) = empty();
        let (mov, into) = copy(&mut func, &mut names, block);
        let load = op(&mut names, "mov_rm_64");
        let mut set = Changes::new();
        set.rewrite(
            mov,
            Plan {
                opcode: load,
                operands: vec![mir::Operand::write(into, GPR)],
                imm: None,
                amode: None,
                symbol: None,
            },
        );

        assert_eq!(refused(&func, &names, &set), Some(Refusal::Mem(mov)));
    }

    /// A scale this machine cannot write. Folding an address into a reader is where a number like
    /// this is worked out, and three is what a multiplication by three looks like halfway through
    /// the fold.
    #[test]
    fn an_index_scaled_by_something_this_machine_cannot_write_is_refused() {
        let (mut names, mut func, block) = empty();
        let (mov, into) = copy(&mut func, &mut names, block);
        let base = func.new_vreg(GPR);
        let index = func.new_vreg(GPR);
        let load = op(&mut names, "mov_rm_64");
        let scaled = |scale| Plan {
            opcode: load,
            operands: vec![
                mir::Operand::write(into, GPR),
                mir::Operand::read(base, GPR),
                mir::Operand::read(index, GPR),
            ],
            imm: None,
            amode: Some(mir::Amode {
                base: Some(1),
                index: Some(2),
                scale,
                disp: 0,
                symbol: None,
                block: None,
                reach: mir::Reach::Itself,
                segment: None,
            }),
            symbol: None,
        };
        let mut set = Changes::new();
        set.rewrite(mov, scaled(3));
        assert_eq!(refused(&func, &names, &set), Some(Refusal::Scale(mov)));

        let mut set = Changes::new();
        set.rewrite(mov, scaled(4));
        assert_eq!(refused(&func, &names, &set), None);
    }

    /// An address whose base points at an operand the description already claimed. The registers an
    /// address names come after the ones the instruction itself has, and a mode pointing into the
    /// middle of the others is a printer's mistake waiting to happen.
    #[test]
    fn an_address_pointing_at_an_operand_of_its_own_instruction_is_refused() {
        let (mut names, mut func, block) = empty();
        let (mov, into) = copy(&mut func, &mut names, block);
        let load = op(&mut names, "mov_rm_64");
        let mut set = Changes::new();
        set.rewrite(
            mov,
            Plan {
                opcode: load,
                operands: vec![mir::Operand::write(into, GPR)],
                imm: None,
                amode: Some(mir::Amode {
                    base: Some(0),
                    index: None,
                    scale: 1,
                    disp: 0,
                    symbol: None,
                    block: None,
                    reach: mir::Reach::Itself,
                    segment: None,
                }),
                symbol: None,
            },
        );

        assert_eq!(refused(&func, &names, &set), Some(Refusal::Operands(mov)));
    }

    /// The question the set is for. Taking an instruction out while something still reads what it
    /// wrote is the mistake every pass that removes instructions can make, and this is the one
    /// place it is answered.
    #[test]
    fn removing_an_instruction_whose_answer_is_still_read_is_refused() {
        let (mut names, mut func, block) = empty();
        let (mov, into) = copy(&mut func, &mut names, block);
        let out = func.new_vreg(GPR);
        let second = func[mov].opcode;
        func.build(block, second).def(out, GPR).uses(into, GPR).finish();
        let mut set = Changes::new();
        set.remove(mov);

        assert_eq!(refused(&func, &names, &set), Some(Refusal::Read(mov)));
        assert_eq!(shape(&func, &names, block).len(), 2);
    }

    /// The same removal in a set that deals with the reader as well. This is what a fold is: the
    /// address goes because the instruction that read it does not read it any more, and neither
    /// half is worth doing without the other.
    #[test]
    fn removing_it_in_a_set_that_takes_away_the_reader_is_taken() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let address = func.new_vreg(GPR);
        let out = func.new_vreg(GPR);
        let lea = op(&mut names, "lea_64");
        let load = op(&mut names, "mov_rm_64");
        let at = |reg| mir::Mem { disp: 16, ..mir::Mem::at(mir::Operand::read(reg, GPR)) };
        let made = func.build(block, lea).def(address, GPR).mem(at(base)).finish();
        let read = func
            .build(block, load)
            .def(out, GPR)
            .mem(mir::Mem::at(mir::Operand::read(address, GPR)))
            .finish();

        let mut set = Changes::new();
        set.rewrite(
            read,
            Plan {
                opcode: load,
                operands: vec![mir::Operand::write(out, GPR), mir::Operand::read(base, GPR)],
                imm: None,
                amode: Some(mir::Amode { disp: 16, ..func[func[read].mem.expect("a load")] }),
                symbol: None,
            },
        );
        set.remove(made);

        let mut reads = Reads::of(&func);
        assert_eq!(set.commit(&mut func, &mut reads, &names, &MACHINE), Ok(2));

        assert_eq!(shape(&func, &names, block), ["x64.mov_rm_64"]);
        assert_eq!(reads.count(address), 0, "nothing reads the address the lea wrote");
        assert_eq!(reads.count(base), 1, "the load reads what the lea read");
    }

    /// An argument an edge carries is a read like any other and is in no operand vector, which is
    /// the one place a count of reads is easy to get wrong.
    #[test]
    fn an_answer_an_edge_carries_keeps_its_instruction() {
        let (mut names, mut func, block) = empty();
        let next = func.create_block();
        let (mov, into) = copy(&mut func, &mut names, block);
        let arrived = func.new_vreg(GPR);
        func.params_mut(next).push(mir::Param { reg: arrived, class: GPR });
        *func.succs_mut(block) = vec![mir::BlockCall::with(next, vec![into])];
        let mut set = Changes::new();
        set.remove(mov);

        assert_eq!(refused(&func, &names, &set), Some(Refusal::Read(mov)));
    }

    /// One instruction, two minds. A set that says an instruction becomes one thing and then
    /// another is a pass that has lost track of what it proposed, and taking either half would be
    /// picking for it.
    #[test]
    fn an_instruction_named_twice_is_refused() {
        let (mut names, mut func, block) = empty();
        let (mov, _) = copy(&mut func, &mut names, block);
        let mut set = Changes::new();
        set.rewrite(mov, Plan::of(&func, mov));
        set.remove(mov);

        assert_eq!(refused(&func, &names, &set), Some(Refusal::Twice(mov)));
    }

    /// A set built against a function that has moved on since. The instruction it names is gone,
    /// and a rewrite of an instruction in no block would be a rewrite nothing ever runs.
    #[test]
    fn an_instruction_that_has_already_gone_is_refused() {
        let (mut names, mut func, block) = empty();
        let (mov, _) = copy(&mut func, &mut names, block);
        let plan = Plan::of(&func, mov);
        func.remove_inst(mov);
        let mut set = Changes::new();
        set.rewrite(mov, plan);

        assert_eq!(refused(&func, &names, &set), Some(Refusal::Gone(mov)));
    }

    /// The instruction as it stands is a proposal that changes nothing, which is what a pass that
    /// rewrites one operand starts from.
    #[test]
    fn the_instruction_as_it_stands_is_a_proposal_the_target_takes() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let out = func.new_vreg(GPR);
        let load = op(&mut names, "mov_rm_64");
        let read = func
            .build(block, load)
            .def(out, GPR)
            .mem(mir::Mem { disp: 24, ..mir::Mem::at(mir::Operand::read(base, GPR)) })
            .finish();

        let plan = Plan::of(&func, read);
        let mut set = Changes::new();
        set.rewrite(read, plan.clone());
        let mut reads = Reads::of(&func);
        assert_eq!(set.commit(&mut func, &mut reads, &names, &MACHINE), Ok(1));

        assert_eq!(Plan::of(&func, read), plan);
        assert_eq!(reads.count(base), 1, "the one read it had before");
    }

    /// A refused set writes nothing, including the half of it that would have been fine. That is
    /// the whole point of proposing a set rather than applying instructions one at a time.
    #[test]
    fn a_set_with_one_bad_change_in_it_leaves_the_others_alone() {
        let (mut names, mut func, block) = empty();
        let (first, into) = copy(&mut func, &mut names, block);
        let (second, _) = copy(&mut func, &mut names, block);
        let load = op(&mut names, "mov_rm_64");
        let mut set = Changes::new();
        set.rewrite(
            first,
            Plan {
                opcode: load,
                operands: vec![mir::Operand::write(into, GPR), mir::Operand::read(into, GPR)],
                imm: None,
                amode: Some(mir::Amode {
                    base: Some(1),
                    index: None,
                    scale: 1,
                    disp: 0,
                    symbol: None,
                    block: None,
                    reach: mir::Reach::Itself,
                    segment: None,
                }),
                symbol: None,
            },
        );
        set.rewrite(second, Plan { imm: Some(3), ..Plan::of(&func, second) });

        let mut reads = Reads::of(&func);
        assert_eq!(
            set.commit(&mut func, &mut reads, &names, &MACHINE),
            Err(Refusal::Imm(second)),
            "the second change is the one the target turns down"
        );
        assert_eq!(shape(&func, &names, block), ["x64.mov_rr_64", "x64.mov_rr_64"]);
    }

    /// A set with nothing in it, which is what a pass that found nothing to do hands over.
    #[test]
    fn a_set_with_nothing_in_it_commits() {
        let (mut names, mut func, block) = empty();
        copy(&mut func, &mut names, block);
        let set = Changes::new();
        assert!(set.is_empty());

        let mut reads = Reads::of(&func);
        assert_eq!(set.commit(&mut func, &mut reads, &names, &MACHINE), Ok(0));
        assert_eq!(shape(&func, &names, block), ["x64.mov_rr_64"]);
    }

    /// The counts a pass carries from one commit to the next. A removal the first commit makes
    /// possible has to be a removal the second commit agrees to, and it only is if the count came
    /// down when the reader went.
    #[test]
    fn the_counts_are_still_right_after_a_commit() {
        let (mut names, mut func, block) = empty();
        let (first, into) = copy(&mut func, &mut names, block);
        let out = func.new_vreg(GPR);
        let mov = func[first].opcode;
        let second = func.build(block, mov).def(out, GPR).uses(into, GPR).finish();

        let mut reads = Reads::of(&func);
        assert_eq!(reads.count(into), 1);
        let mut set = Changes::new();
        set.remove(second);
        assert_eq!(set.commit(&mut func, &mut reads, &names, &MACHINE), Ok(1));
        assert_eq!(reads.count(into), 0, "the reader went and the count went with it");

        let mut set = Changes::new();
        set.remove(first);
        assert_eq!(set.commit(&mut func, &mut reads, &names, &MACHINE), Ok(1));
        assert!(shape(&func, &names, block).is_empty());
    }
}
