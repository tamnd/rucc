//! Taking out a move the allocator wrote that puts a value where the machine has it already.
//!
//! Design: `spec/optimizer/37-machine-level-optimization.md` sections 37.4 and 37.6, the group of
//! passes that run after allocation and clean up what the allocator could not.
//!
//! The allocator decides one value at a time. It writes a value out when the range it was given a
//! register for ends, it reads a value back in front of the instruction that wants it, and neither
//! decision looks at the other. So a value written by one instruction and wanted by the next comes
//! out as a store and then the load of the same slot on the very next line:
//!
//! ```text
//!   movq %r10, 16(%rsp)
//!   movq 16(%rsp), %r10
//! ```
//!
//! The load reads a word the store has just written, into the register the store read it out of,
//! so the register already holds what the load would put in it. It is a memory access that cannot
//! change anything, on the two instructions of the pair that are the expensive one.
//!
//! The same thing happens with instructions in between. A value spilled once and read back at
//! three places in a block is three loads of one slot into one scratch register, and the second
//! and the third are reads of a word that register still holds. A value read back and then written
//! out again unchanged is a store of a word the slot still holds. A copy into a register that
//! already holds what it is being given is a copy of nothing. All of them are the same mistake
//! seen from different sides, which is why they are one pass rather than four: a move is dead when
//! what it writes to and what it reads from hold the same value already.
//!
//! The near miss of the same thing is a load of a slot into a register while a different register
//! holds that word. Nothing can be removed there, since the word does have to arrive in the
//! register the load names, but it can come out of the register that has it rather than out of the
//! frame. That is the second thing this does and the rest of the same knowledge answers it.
//!
//! # Why it is not a rule over the instructions
//!
//! A store followed by a load of the same address is not on its own a dead load. The same pair of
//! instructions is what a write to a local variable and a read of it back look like, and when that
//! variable is `volatile` the read is one the program insisted on and the standard says happens.
//! Machine IR carries that word now, on the instruction the access was selected from, so the
//! question could be asked here. It is still the wrong question to build the pass on, because
//! `volatile` is not the only reason a read of a place the program named has to stay.
//!
//! So this does not look for the pattern. [`crate::finish`] records which instruction each of the
//! allocator's moves became, and this pass only ever takes out one of those. A spill slot belongs
//! to the allocator, nothing else reads it or writes it, and no part of the program said anything
//! about it, which is what makes removing a read of one safe when removing a read of a variable is
//! not.
//!
//! A slot can end up sharing its bytes with a local, which [`crate::slots`] arranges, and that does
//! not change the argument. Two things share a run of the frame only where they are never both
//! wanted, and a slot between a spill and the read back of it is wanted the whole way, so a store
//! to the local it shares with cannot fall in that stretch.
//!
//! # Why after the whole allocator rather than inside it
//!
//! The edits are decided in different places for different reasons, so no one of the decisions is
//! wrong on its own and there is no place inside the allocator that sees them together. What ends
//! up between two of them is settled by [`crate::finish`] writing every edit into the function,
//! which is after the allocator has finished. They are all visible at once here and nowhere
//! earlier.
//!
//! # What a block is walked with
//!
//! One map from a place to a number standing for a value, where a place is a register of a class
//! or a slot of one. Two places with the same number hold the same bits, and that is the whole of
//! what the pass knows: nothing here has to know what the value is, only that a move of one place
//! into another with the same number would write what is there.
//!
//! The map starts empty at the top of every block, because what a register holds on the way in is
//! whatever the block before it left, and which block that is depends on the path. A map carried
//! across edges would be worth something on a straight line of blocks and is a dataflow problem
//! rather than a walk, which is section 37.4's own answer for why this is the cheap half.
//!
//! # What clears it
//!
//! A write to a place clears what was there, which is every definition in an operand vector and
//! the destination of every move.
//!
//! A call clears everything. The registers a convention does not preserve are gone across one,
//! and the ones the arguments travelled in are written down as reads rather than as writes, so
//! the operand vector of a call does not say what it destroys. That is why the machine
//! description is asked which instructions are calls rather than the operands being trusted.
//!
//! An instruction the description does not name clears everything too, on the same reasoning
//! backwards: what a pass cannot look up it cannot claim to have read.
//!
//! An instruction that writes the stack pointer or the frame pointer clears everything, because a
//! slot is named by an offset from one of those and a slot at a new address is not the slot the
//! value was written to. A prologue and an epilogue do this, which costs nothing since neither is
//! in the middle of anything, and so does the instruction that takes room for an array whose size
//! is not known until it runs.
//!
//! # When a load becomes a copy
//!
//! A slot read into a register while no register holds that word is a load and has to stay one. A
//! slot read into one register while another register holds the same word is a load a copy would
//! do instead, and a copy is the cheaper of the two on every machine here: it is fewer bytes, it
//! does not go near the memory unit, and on a machine that renames its registers it often costs
//! nothing to run at all.
//!
//! Which instruction that copy is is the target's answer and not this pass's, and it is the same
//! answer [`crate::finish`] read to write the load. A class of registers is moved between two
//! registers by one named instruction and between a register and the frame by two others, and the
//! three are named together for that class, so asking for the one is asking the description that
//! produced the other.
//!
//! Being named together is also what makes the widths agree. Every entry in the map was put there
//! by one of the allocator's own moves and every one of those moves a whole register of its class,
//! so two places holding one value hold it in all of their bytes rather than in a low part that a
//! wider copy would read past.
//!
//! Where several registers hold the word, the lowest numbered of them is the one written. Any of
//! them would be correct, and the map is a hash map, so writing whichever came out of it first
//! would make the assembly depend on where the addresses happened to land. A compiler whose output
//! moves between two runs of one input is one nobody can compare anything against, which is a
//! worse thing to be than one that picks the second best register.
//!
//! # Why it does not go through the change framework
//!
//! Because the one question [`crate::changes`] would answer about a removal is one that cannot be
//! answered here. The framework refuses to take an instruction out while anything still reads a
//! register it wrote, and it knows that from a count of the reads in the function, which is the
//! whole answer while every register is written once and is not the answer at all afterwards: the
//! register a reload writes is physical by the time this runs, the same one is written and read all
//! over the function about other values, and the count says so. Every removal here would be turned
//! down.
//!
//! What makes these safe is not a count but where the instruction came from. It is one of the
//! allocator's own moves, [`crate::finish`] wrote it, and the value is in the register already
//! because an earlier move of the allocator's put it there. That is a reason the framework has no
//! way to be told, and section 37.2's framework is about the machine's description of itself
//! rather than about the allocator's, so this keeps its own.
//!
//! The framework's own question, whether the target has an instruction of the shape a pass
//! proposes, is one the rewrite does not have to ask either. The copy it writes is the instruction
//! the description names for moving a register of that class, so it is an instruction the target
//! has by where the name came from rather than by a lookup afterwards.
//!
//! # A copy of a register into itself
//!
//! One more thing goes here, and it is not one of the allocator's moves: a copy a pass in front of
//! the allocator wrote, whose two ends the allocator then gave one register. [`itself`] takes those
//! out, and it needs none of the above to do it. The instruction is the one the target names for
//! moving a whole register of the class, it reads the register it writes, and so what it writes is
//! what is there by its operands alone.
//!
//! # What it does not do
//!
//! Nothing is propagated. A read of a register that another register is known to equal stays a
//! read of the register it names. The two things [`clean`] does are both to one of the allocator's
//! own moves and to nothing else, for the reason the rest of this is built on: what makes an edit
//! there safe is that the allocator wrote the instruction and owns the slot, and an instruction a
//! lowering rule wrote is neither.
//!
//! Nothing crosses a block, on either half.

use rucc_base::Interner;
use rucc_base::hash::{Map, Set};
use rucc_mir::{Block, Constraint, Func, Inst, Opcode, Operand, Reg, Role};
use rucc_regalloc::assign::Place;
use rucc_regalloc::rewrite::Edit;
use rucc_target::{CallRegs, FrameInsts, MachineInsts, PhysReg, RegClass};

use crate::changes::Plan;
use crate::combine::{A64_WIDENINGS, FOLDS, WIDENINGS};
use crate::finish::Moves;

/// What one function came to.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Cleaned {
    /// Moves taken out, because the place each wrote held what it was moving already.
    pub gone: usize,
    /// Loads out of the frame written as copies instead, because a register held the word.
    pub copied: usize,
}

/// Takes out every move of the allocator's that puts a value where it is already, and reads the
/// rest out of a register wherever one has the word the frame does.
///
/// # Panics
///
/// Panics on a move of a class the target did not say how to move, which is the same frame
/// description [`crate::finish`] wrote the move out of and so is the caller handing this a
/// function and a target that were not worked out from each other.
pub fn clean(
    func: &mut Func,
    moves: &Moves,
    machine: &MachineInsts,
    frame: &FrameInsts,
    conv: &CallRegs,
    framed: bool,
    names: &mut Interner,
) -> Cleaned {
    let blocks: Vec<Block> = func.blocks().collect();
    let mut cleaned = Cleaned::default();
    // Whether each opcode is a call or one the target does not have, asked once per opcode rather
    // than by name for every instruction. tamnd/rucc#2233.
    let mut stops: Map<Opcode, bool> = Map::default();
    for block in blocks {
        let insts: Vec<Inst> = func.insts(block).collect();
        let mut holds = Holds::default();
        let mut gone: Vec<Inst> = Vec::new();
        for inst in insts {
            // One of the allocator's own moves, which is the only kind of instruction this edits
            // and the only kind it learns anything from.
            if let Some(edit) = moves.at(inst) {
                if holds.same(edit.class, edit.mov.to, edit.mov.from) {
                    gone.push(inst);
                    cleaned.gone += 1;
                    continue;
                }
                // A load of a word a register has. The copy goes where the load was and the load
                // goes, and what arrives in the register is the same either way, so the map is
                // told about the move below whichever of the two instructions is left.
                if let Some((to, from)) = instead(&holds, &edit) {
                    let copy = copy(func, names, frame, edit.class, to, from);
                    func.insert_before(inst, copy);
                    gone.push(inst);
                    cleaned.copied += 1;
                }
                holds.moved(edit.class, edit.mov.to, edit.mov.from);
                continue;
            }
            let opcode = func[inst].opcode;
            let stop = *stops.entry(opcode).or_insert_with(|| {
                let name = names.resolve(opcode.name());
                machine.calls(name) || !machine.has(name)
            });
            if stop {
                holds.nothing();
                continue;
            }
            let mut addressing = false;
            for operand in &func[func[inst].operands] {
                if !operand.role.is_def() {
                    continue;
                }
                let Some(reg) = operand.reg.phys() else { continue };
                addressing |= addresses(conv, framed, operand.class, reg);
                holds.wrote(operand.class, Place::Reg(reg));
            }
            if addressing {
                holds.nothing();
            }
        }
        for inst in gone {
            func.remove_inst(inst);
        }
    }
    cleaned
}

/// Takes out every store of the allocator's into a slot that none of its moves reads any more, and
/// gives back those slots.
///
/// [`clean`] takes out a read of a slot whose word a register still holds, and when it has taken
/// out every read of a slot the stores into it are left writing a word nobody asks for. A row of
/// `cmov`s on i386 that ran short of registers for a moment kept one of its values in `esi` the
/// whole way and still wrote it to the frame at the top, because the allocator had planned to read
/// it back from there and the cleanup found every one of those reads already in `esi`.
///
/// A spill slot is read by nothing but the allocator's own moves, which is the argument [`clean`]
/// is built on, so a slot none of them reads is one nothing reads. An operand the allocator was
/// told has to be on the stack is the exception, since the instruction reads the slot itself, and
/// a function with one is left as it is. The slots come back for the debug information, which
/// would otherwise say a value is in a slot nothing wrote.
pub fn unread(func: &mut Func, moves: &Moves) -> Vec<u32> {
    let mut read: Set<u32> = Set::default();
    let mut stores: Vec<(Inst, u32)> = Vec::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            let Some(edit) = moves.at(inst) else {
                if func[func[inst].operands]
                    .iter()
                    .any(|operand| operand.constraint == Constraint::Stack)
                {
                    return Vec::new();
                }
                continue;
            };
            if let Place::Slot(slot) = edit.mov.from {
                read.insert(slot);
            }
            if let Place::Slot(slot) = edit.mov.to {
                stores.push((inst, slot));
            }
        }
    }
    let mut gone: Vec<u32> = Vec::new();
    for (inst, slot) in stores {
        if !read.contains(&slot) {
            func.remove_inst(inst);
            gone.push(slot);
        }
    }
    gone.sort_unstable();
    gone.dedup();
    gone
}

/// Takes out every copy of a register into itself, and gives back how many it took out.
///
/// The allocator never writes one of these, since a move between two places that are the same
/// place is one it leaves out, but a pass in front of it can. [`crate::split::indirect`] writes a
/// copy of each value a computed `goto` carries into a register of its own, in front of the jump,
/// and the allocator then hands that register the one the value was in already, because the value
/// dies at the copy. What is left is `movq %rax, %rax` for every value an interpreter's dispatch
/// carries, in front of every jump of the dispatch, which is most of the instructions such a loop
/// runs that are not the loop's own. tamnd/rucc#1994.
///
/// Only the instruction the target names for moving a register of a class is taken, and only
/// where it reads and writes the one register of that class. That instruction moves the whole of
/// the register on every target here, so writing a register with what it holds changes nothing.
/// A narrower move is left alone even with both ends the same, because on x86-64 `movl %eax, %eax`
/// clears the top half and a program may be counting on it.
///
/// This is not the reasoning the allocator's own moves are taken out on. Nothing is known about
/// what any register holds, nothing is read out of the frame, and a copy that writes what is there
/// by its operands alone needs no record of where it came from to be safe to take out.
pub fn itself(func: &mut Func, frame: &FrameInsts, names: &mut Interner) -> usize {
    // The move of each class by the number of the class, asked once per class rather than by name
    // for every instruction.
    let mut movs: Map<u8, Option<Opcode>> = Map::default();
    let mut gone: Vec<Inst> = Vec::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            if func[inst].mem.is_some() {
                continue;
            }
            let [to, from] = &func[func[inst].operands] else { continue };
            let same = to.role.is_def()
                && !from.role.is_def()
                && to.class == from.class
                && to.reg.phys().is_some()
                && to.reg == from.reg;
            if !same {
                continue;
            }
            let class = to.class;
            let mov = *movs.entry(class.number()).or_insert_with(|| {
                let moves = frame.moves(class)?;
                Some(Opcode::new(names.join(frame.prefix, moves.mov)))
            });
            if mov == Some(func[inst].opcode) {
                gone.push(inst);
            }
        }
    }
    for &inst in &gone {
        func.remove_inst(inst);
    }
    gone.len()
}

/// Reads a spilled value out of the frame in the instruction that wanted it, where the allocator
/// read it into a scratch register in front of that instruction, and gives back how many.
///
/// ```text
///   movq 40(%rsp), %r10
///   cmpl %r10d, %r13d      ->    cmpl 40(%rsp), %r13d
/// ```
///
/// [`crate::combine::loads`] does this for a load the selector wrote, and cannot do it for one the
/// allocator wrote, which is not there until the allocator is done. The hot loop of Postgres'
/// tuple deforming reads its bound back like that on every turn, and gcc reads it out of the frame
/// in the comparison. tamnd/rucc#1994.
///
/// Only a load the allocator asked for, which is what `moves` says, and only into a scratch
/// register of the class `scratch` names. Nothing lives in one of those into another block, so
/// whether the value is wanted past the instruction is a question about the rest of the block. A
/// register the allocator handed out is not one, even where it is held back from other functions:
/// a load into it may be a value coming back from where it waited around a call, and that value
/// can be wanted in the next block. Only the allocator's moves
/// into registers may stand between the load and the instruction, since none of them writes
/// memory, and none of them may write the register the load did.
///
/// The load may be wider than the instruction reads. A slot holds the whole register it was
/// spilled from, and on a machine that keeps the low bytes first the narrower read of the same
/// address is the low part of that register, which is what the instruction would have read.
pub fn reloads(
    func: &mut Func,
    moves: &Moves,
    machine: &MachineInsts,
    scratch: (RegClass, &[PhysReg]),
    names: &mut Interner,
) -> usize {
    let mut done = 0;
    for block in func.blocks().collect::<Vec<_>>() {
        let insts: Vec<Inst> = func.insts(block).collect();
        for (at, &load) in insts.iter().enumerate() {
            let Some(edit) = moves.at(load) else { continue };
            let (Place::Reg(reg), Place::Slot(_)) = (edit.mov.to, edit.mov.from) else { continue };
            let class = edit.class;
            if class != scratch.0 || !scratch.1.contains(&reg) {
                continue;
            }
            let Some(user) = reader(func, moves, &insts[at + 1..], class, reg) else { continue };
            if wanted(func, &insts[at + 1..], user, class, reg) {
                continue;
            }
            let Some(plan) = reloaded(func, machine, names, load, user, (class, reg)) else {
                continue;
            };
            let operands = func.push_operands(&plan.operands);
            let imm = plan.imm.map(|value| func.add_imm(value));
            let mem = plan.amode.map(|amode| func.add_amode(amode));
            let data = &mut func[user];
            data.opcode = plan.opcode;
            data.operands = operands;
            data.imm = imm;
            data.mem = mem;
            data.symbol = plan.symbol;
            func.remove_inst(load);
            done += 1;
        }
    }
    done
}

/// Reads a scratch register's copy of another register out of that register, and takes the copy
/// out.
///
/// [`clean`] turns a reload of a word another register holds into a copy of that register. That
/// saves the memory access but still spends an instruction, and on i386 a spilled pointer used by
/// two stores in a row comes out as a reload into `edi` for the first and `movl %edi, %esi` for
/// the second. The second store can read `edi` itself, and so can every reader up to the next
/// write of the scratch register, as long as nothing writes the register it was copied from first.
///
/// Only into a scratch register of the class, for the reason [`reloads`] gives: nothing lives in
/// one into another block, so its readers are the rest of the block. A reader that writes the
/// scratch register too, or insists on it, keeps the copy. So does a read behind a call or an
/// instruction the target does not have, since what those read and write is not all in their
/// operands.
pub fn forward(
    func: &mut Func,
    machine: &MachineInsts,
    frame: &FrameInsts,
    scratch: (RegClass, &[PhysReg]),
    names: &Interner,
) -> usize {
    let (class, held) = scratch;
    let Some(moves) = frame.moves(class) else { return 0 };
    let mov = format!("{}{}", frame.prefix, moves.mov);
    let mut done = 0;
    for block in func.blocks().collect::<Vec<_>>() {
        let passed = func[block]
            .succs
            .iter()
            .flat_map(|call| &call.args)
            .any(|arg| arg.phys().is_some_and(|reg| held.contains(&reg)));
        if passed {
            continue;
        }
        let insts: Vec<Inst> = func.insts(block).collect();
        for (at, &copy) in insts.iter().enumerate() {
            let data = &func[copy];
            if names.resolve(data.opcode.name()) != mov || data.mem.is_some() || data.imm.is_some()
            {
                continue;
            }
            let &[to, from] = &func[data.operands][..] else { continue };
            let (Some(into), Some(out)) = (to.reg.phys(), from.reg.phys()) else { continue };
            if !to.role.is_def()
                || from.role.is_def()
                || to.class != class
                || from.class != class
                || into == out
                || !held.contains(&into)
            {
                continue;
            }
            let after = &insts[at + 1..];
            let Some(found) = readers(func, machine, names, after, class, (into, out)) else {
                continue;
            };
            for (inst, index) in found {
                let list = func[inst].operands;
                func[list][index].reg = Reg::physical(out);
            }
            func.remove_inst(copy);
            done += 1;
        }
    }
    done
}

/// Every operand behind a copy that reads the scratch register it went into, up to the next write
/// of that register, or `None` where one of them cannot read the register the copy came from.
fn readers(
    func: &Func,
    machine: &MachineInsts,
    names: &Interner,
    after: &[Inst],
    class: RegClass,
    (into, out): (PhysReg, PhysReg),
) -> Option<Vec<(Inst, usize)>> {
    let is =
        |operand: &Operand, reg: PhysReg| operand.class == class && operand.reg.phys() == Some(reg);
    let mut found = Vec::new();
    // Whether the register the copy came from may hold something else by now.
    let mut changed = false;
    for &inst in after {
        let operands = &func[func[inst].operands];
        let name = names.resolve(func[inst].opcode.name());
        let stop = machine.calls(name) || !machine.has(name);
        let writes = operands.iter().any(|operand| operand.role.is_def() && is(operand, into));
        let early =
            operands.iter().any(|operand| operand.role == Role::EarlyDef && is(operand, out));
        for (index, operand) in operands.iter().enumerate() {
            if operand.role.is_def() || !is(operand, into) {
                continue;
            }
            if changed
                || stop
                || writes
                || early
                || matches!(operand.constraint, Constraint::Fixed(_))
            {
                return None;
            }
            found.push((inst, index));
        }
        if writes {
            return Some(found);
        }
        changed |= stop || operands.iter().any(|operand| operand.role.is_def() && is(operand, out));
    }
    Some(found)
}

/// The first instruction behind a load into that register that reads it, where everything before
/// it is one of the allocator's moves into some other register.
fn reader(
    func: &Func,
    moves: &Moves,
    after: &[Inst],
    class: RegClass,
    reg: PhysReg,
) -> Option<Inst> {
    for &inst in after {
        if touches(func, inst, false, class, reg) {
            return Some(inst);
        }
        let edit = moves.at(inst)?;
        let Place::Reg(to) = edit.mov.to else { return None };
        if edit.class == class && to == reg {
            return None;
        }
    }
    None
}

/// Whether anything behind that instruction reads the register before something writes it.
///
/// The instruction itself counts as the write when it is one. Only the rest of the block is
/// looked at, which is enough for a scratch register and for nothing else.
fn wanted(func: &Func, after: &[Inst], user: Inst, class: RegClass, reg: PhysReg) -> bool {
    if touches(func, user, true, class, reg) {
        return false;
    }
    for &inst in after.iter().skip_while(|&&inst| inst != user).skip(1) {
        if touches(func, inst, false, class, reg) {
            return true;
        }
        if touches(func, inst, true, class, reg) {
            return false;
        }
    }
    false
}

/// Whether that instruction writes the register, or reads it, by the role asked about.
fn touches(func: &Func, inst: Inst, def: bool, class: RegClass, reg: PhysReg) -> bool {
    func[func[inst].operands].iter().any(|operand| {
        operand.role.is_def() == def && operand.class == class && operand.reg.phys() == Some(reg)
    })
}

/// What that instruction becomes with the load in it, where it is a row of the fold tables and
/// the load is one they name at least as wide as the row reads.
///
/// The operands are matched the way [`crate::combine::loads`] matches them, with the one more
/// condition a register that was handed out brings: an answer tied to a source has to be in the
/// register of the source that is kept, which it is not where the load fed that source.
fn reloaded(
    func: &Func,
    machine: &MachineInsts,
    names: &mut Interner,
    load: Inst,
    user: Inst,
    (class, reg): (RegClass, PhysReg),
) -> Option<Plan> {
    if func[user].mem.is_some() {
        return None;
    }
    let width = |name: &str| name.rsplit('_').next()?.parse::<u32>().ok();
    let rows = || FOLDS.iter().chain(WIDENINGS).chain(A64_WIDENINGS);
    let had = machine.bare(names.resolve(func[load].opcode.name())).to_owned();
    if !rows().any(|fold| fold.load == had) {
        return None;
    }
    let bare = machine.bare(names.resolve(func[user].opcode.name())).to_owned();
    let fold = rows().find(|fold| fold.from == bare)?;
    if width(&had)? < width(fold.load)? {
        return None;
    }
    let is = |operand: &Operand| operand.class == class && operand.reg.phys() == Some(reg);
    let operands = func[func[user].operands].to_vec();
    let (front, into) = match operands[..] {
        [answer, first, second] => {
            let (kept, into) = if is(&second) {
                (first, fold.into)
            } else if is(&first) {
                (second, fold.swapped?)
            } else {
                return None;
            };
            let tied = matches!(answer.constraint, Constraint::Reuse(_));
            if is(&kept) || (tied && answer.reg != kept.reg) {
                return None;
            }
            (vec![answer, kept], into)
        }
        [answer, only] if is(&only) => (vec![answer], fold.into),
        _ => return None,
    };
    let into = format!("{}{}", machine.prefix, into);
    if !machine.has(&into) {
        return None;
    }
    let address = func[func[load].operands][1..].to_vec();
    let mut amode = func[func[load].mem?];
    let along = u8::try_from(front.len() - 1).expect("a handful of operands");
    amode.base = amode.base.map(|at| at + along);
    amode.index = amode.index.map(|at| at + along);
    Some(Plan {
        opcode: Opcode::new(names.intern(&into)),
        operands: front.into_iter().chain(address).collect(),
        imm: func[user].imm.map(|at| func[at].0),
        amode: Some(amode),
        symbol: func[load].symbol,
    })
}

/// Whether that register is one the frame is addressed through, so writing it moves every slot.
///
/// The frame pointer only in a frame that keeps one. Elsewhere it is a register the allocator
/// handed out like any other, and a write of it is a value arriving. tamnd/rucc#2777.
fn addresses(conv: &CallRegs, framed: bool, class: RegClass, reg: PhysReg) -> bool {
    class == conv.int_class && (reg == conv.stack_pointer || (framed && reg == conv.frame_pointer))
}

/// The two registers a copy would be written between, where this edit is a load out of the frame
/// of a word some register holds.
///
/// `None` for every other edit. A store has nowhere else to go, since the word has to reach the
/// frame and no machine here writes the frame from anywhere but a register, and a copy between two
/// registers is already the instruction this would be turning something into.
fn instead(holds: &Holds, edit: &Edit) -> Option<(PhysReg, PhysReg)> {
    let (Place::Reg(to), Place::Slot(_)) = (edit.mov.to, edit.mov.from) else { return None };
    Some((to, holds.register(edit.class, edit.mov.from)?))
}

/// The instruction that copies a register of that class into another on this target.
fn copy(
    func: &mut Func,
    names: &mut Interner,
    frame: &FrameInsts,
    class: RegClass,
    to: PhysReg,
    from: PhysReg,
) -> Inst {
    let moves = frame.moves(class).expect("a class the target says how to move");
    let mov = Opcode::new(names.join(frame.prefix, moves.mov));
    func.build_loose(mov).def(Reg::physical(to), class).uses(Reg::physical(from), class).finish()
}

/// Which places are known to hold the same value as each other, over one block.
///
/// A value is a number and nothing more. Where it came from and what it means are questions this
/// does not ask, because the only thing a move is taken out over is two places holding the same
/// one.
#[derive(Debug, Default)]
struct Holds {
    /// What is in each place, by the class it is a place of and the place itself, hashed with
    /// [`rucc_base::hash::Mix`] because this is asked about every register every instruction
    /// writes. A class is in the key because a register is a number inside its class and a slot
    /// is a slot of one, so number four of one file and number four of another are two places.
    what: Map<(u8, Place), u32>,
    /// How many values have been named, so the next one is a number no other place holds.
    named: u32,
}

impl Holds {
    /// Whether both places are known to hold one value, which is what makes a move of the one into
    /// the other write what is there.
    fn same(&self, class: RegClass, to: Place, from: Place) -> bool {
        let read = self.what.get(&(class.number(), from));
        read.is_some() && read == self.what.get(&(class.number(), to))
    }

    /// A register known to hold what that place holds, and the lowest numbered of them where more
    /// than one does.
    ///
    /// Lowest numbered rather than whichever the map hands back first, because the map is a hash
    /// map and which entry comes out of one first is a fact about the hash rather than the input.
    /// Reading it would tie the assembly to how the map happens to hash.
    fn register(&self, class: RegClass, place: Place) -> Option<PhysReg> {
        let value = *self.what.get(&(class.number(), place))?;
        self.what
            .iter()
            .filter(|&(&(number, _), &held)| number == class.number() && held == value)
            .filter_map(|(&(_, place), _)| match place {
                Place::Reg(reg) => Some(reg),
                Place::Slot(_) => None,
            })
            .min_by_key(|reg| reg.number())
    }

    /// Records a move that ran, so what it wrote holds what it read.
    ///
    /// A read of a place nothing is known about is what names a value: the bits are whatever they
    /// are, the two ends of the move agree about them from here on, and that agreement is the only
    /// thing this pass ever asks about.
    fn moved(&mut self, class: RegClass, to: Place, from: Place) {
        let value = match self.what.get(&(class.number(), from)) {
            Some(&value) => value,
            None => {
                self.named += 1;
                self.what.insert((class.number(), from), self.named);
                self.named
            }
        };
        self.what.insert((class.number(), to), value);
    }

    /// Records that something wrote a place, so whatever it held is no longer what is there.
    fn wrote(&mut self, class: RegClass, place: Place) {
        self.what.remove(&(class.number(), place));
    }

    /// Forgets the block so far, which is the answer to an instruction whose writes cannot all be
    /// seen.
    fn nothing(&mut self) {
        self.what.clear();
    }
}

#[cfg(test)]
mod tests {
    use rucc_mir::{Constraint, Mem, Opcode, Operand, Reg};
    use rucc_regalloc::moves::Move;
    use rucc_regalloc::rewrite::{At, Edit};
    use rucc_target::x86_64::{FRAME, GPR, MACHINE, R13, RAX, RBX, SYSV, XMM};

    use super::*;

    /// A spill register to write with, which is the one x86-64 holds back for exactly this.
    const R10: PhysReg = PhysReg::new(10);

    /// The second of them, which is what an instruction with both operands on the stack reads the
    /// other one into.
    const R11: PhysReg = PhysReg::new(11);

    /// A function with one block in it, and the names it was built with.
    fn empty() -> (Interner, Func, Block) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        (names, func, block)
    }

    /// The pass, with the one target these tests are written against.
    fn clean(func: &mut Func, moves: &Moves, names: &mut Interner) -> Cleaned {
        super::clean(func, moves, &MACHINE, &FRAME, &SYSV, true, names)
    }

    /// How many moves went, for a test about the half that removes.
    fn gone(func: &mut Func, moves: &Moves, names: &mut Interner) -> usize {
        clean(func, moves, names).gone
    }

    /// The opcode of that name on this target.
    fn op(names: &mut Interner, name: &str) -> Opcode {
        Opcode::new(names.intern(&format!("{}{name}", FRAME.prefix)))
    }

    /// A store of a physical register to a frame address, as [`crate::finish`] writes a spill.
    fn store(func: &mut Func, names: &mut Interner, block: Block, reg: PhysReg, at: i32) -> Inst {
        let store = op(names, "mov_mr_64");
        let base = Operand::read(Reg::physical(RAX), GPR);
        func.build(block, store).uses(Reg::physical(reg), GPR).mem(Mem::at(base).plus(at)).finish()
    }

    /// A load of a physical register from a frame address, as it writes a reload.
    fn load(func: &mut Func, names: &mut Interner, block: Block, reg: PhysReg, at: i32) -> Inst {
        let load = op(names, "mov_rm_64");
        let base = Operand::read(Reg::physical(RAX), GPR);
        func.build(block, load).def(Reg::physical(reg), GPR).mem(Mem::at(base).plus(at)).finish()
    }

    /// A copy of one physical register into another, as it writes one of the allocator's copies.
    fn copy(
        func: &mut Func,
        names: &mut Interner,
        block: Block,
        to: PhysReg,
        from: PhysReg,
    ) -> Inst {
        let copy = op(names, "mov_rr_64");
        func.build(block, copy).def(Reg::physical(to), GPR).uses(Reg::physical(from), GPR).finish()
    }

    /// An instruction that reads a register and writes another, which is what a spilled value was
    /// read back for.
    fn add(
        func: &mut Func,
        names: &mut Interner,
        block: Block,
        to: PhysReg,
        from: PhysReg,
    ) -> Inst {
        let add = op(names, "add_rr_64");
        func.build(block, add).def(Reg::physical(to), GPR).uses(Reg::physical(from), GPR).finish()
    }

    /// What the allocator asked for, in the two shapes this pass is about.
    ///
    /// Where the edit was to go is not read by anything here, since what says two instructions are
    /// next to each other is the function they were written into rather than what the allocator
    /// said about where they belong.
    fn out(block: Block, slot: u32, reg: PhysReg) -> Edit {
        Edit {
            at: At::StartOf(block),
            mov: Move::new(Place::Slot(slot), Place::Reg(reg)),
            class: GPR,
        }
    }

    /// The other direction, and the one a dead reload is.
    fn back(block: Block, slot: u32, reg: PhysReg) -> Edit {
        Edit {
            at: At::StartOf(block),
            mov: Move::new(Place::Reg(reg), Place::Slot(slot)),
            class: GPR,
        }
    }

    /// A move of one register into another, which the allocator writes where the two ends of a
    /// value could not be given the same register.
    fn across(block: Block, to: PhysReg, from: PhysReg) -> Edit {
        Edit {
            at: At::StartOf(block),
            mov: Move::new(Place::Reg(to), Place::Reg(from)),
            class: GPR,
        }
    }

    /// How many instructions a block has left.
    fn left(func: &Func, block: Block) -> usize {
        func.insts(block).count()
    }

    /// What the block says now, one opcode per instruction, which is how a test about a rewrite
    /// says which instruction came out of it.
    fn written(func: &Func, block: Block, names: &Interner) -> Vec<String> {
        func.insts(block).map(|inst| names.resolve(func[inst].opcode.name()).to_owned()).collect()
    }

    /// The registers the instruction at that position in the block reads.
    fn reads(func: &Func, block: Block, at: usize) -> Vec<PhysReg> {
        let inst = func.insts(block).nth(at).expect("an instruction at that position");
        func[func[inst].operands]
            .iter()
            .filter(|operand| !operand.role.is_def())
            .filter_map(|operand| operand.reg.phys())
            .collect()
    }

    /// The pair the pass started as: a word written out and read straight back into the register it
    /// was written out of.
    #[test]
    fn a_reload_of_the_slot_the_instruction_in_front_of_it_spilled_goes() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 1);
        assert_eq!(left(&func, block), 1, "the spill went too, or the reload stayed");
        assert_eq!(func.insts(block).next(), Some(spill));
    }

    /// The spill of that pair once its reload has gone, since nothing reads the slot after that.
    #[test]
    fn a_spill_whose_every_reload_went_goes_after_them() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R10));
        gone(&mut func, &moves, &mut names);

        assert_eq!(unread(&mut func, &moves), [0]);
        assert_eq!(left(&func, block), 0);
    }

    /// A spill whose slot is still read back somewhere stays, and so does every other store into
    /// that slot.
    #[test]
    fn a_spill_of_a_slot_still_read_stays() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        add(&mut func, &mut names, block, R10, RAX);
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R10));
        gone(&mut func, &moves, &mut names);

        assert!(unread(&mut func, &moves).is_empty());
        assert_eq!(left(&func, block), 3);
    }

    /// Two reads of one word, which is one spill and two reloads, and the second is as dead as the
    /// first because the register still holds what the first put in it.
    #[test]
    fn a_run_of_reloads_of_one_slot_goes_in_a_single_pass() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let first = load(&mut func, &mut names, block, R10, 16);
        let second = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(first, back(block, 0, R10));
        moves.record(second, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 2);
        assert_eq!(left(&func, block), 1);
    }

    /// The case the pass was grown for. What the value was read back for stands between the two
    /// reloads, and it writes neither the slot nor the register the word is in, so the second read
    /// is of a word that register still holds.
    #[test]
    fn a_reload_with_an_instruction_between_that_writes_neither_end_goes() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let first = load(&mut func, &mut names, block, R10, 16);
        add(&mut func, &mut names, block, RAX, R10);
        let second = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(first, back(block, 0, R10));
        moves.record(second, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 2);
        assert_eq!(left(&func, block), 2, "the spill and the instruction between are the two");
    }

    /// The instruction between them writes the register this time, which is what a spilled value
    /// is read back into a scratch register for, so the reload behind it reads a word that
    /// register no longer holds.
    #[test]
    fn a_reload_behind_an_instruction_that_writes_the_register_stays() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        add(&mut func, &mut names, block, R10, RAX);
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 3);
    }

    /// A word read back and written out again with nothing touching either end is a store of what
    /// the slot holds already.
    #[test]
    fn a_spill_of_a_word_the_slot_still_holds_goes() {
        let (mut names, mut func, block) = empty();
        let reload = load(&mut func, &mut names, block, R10, 16);
        let spill = store(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(reload, back(block, 0, R10));
        moves.record(spill, out(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 1);
        assert_eq!(left(&func, block), 1);
        assert_eq!(func.insts(block).next(), Some(reload));
    }

    /// A copy into a register that holds what it is being given writes what is there.
    #[test]
    fn a_copy_of_a_word_the_register_already_holds_goes() {
        let (mut names, mut func, block) = empty();
        let first = copy(&mut func, &mut names, block, RAX, R10);
        let second = copy(&mut func, &mut names, block, RAX, R10);
        let mut moves = Moves::default();
        moves.record(first, across(block, RAX, R10));
        moves.record(second, across(block, RAX, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 1);
        assert_eq!(left(&func, block), 1);
    }

    /// A reload of another slot reads another word, whatever address the two instructions were
    /// written with.
    #[test]
    fn a_reload_of_a_different_slot_stays() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let reload = load(&mut func, &mut names, block, R10, 24);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 1, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 2);
    }

    /// A reload into another register puts the word somewhere it is not, so the instruction has
    /// work to do. Where it takes the word from is the question, and the register that was spilled
    /// still holds it, so the frame is not read.
    #[test]
    fn a_reload_into_a_different_register_becomes_a_copy() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let reload = load(&mut func, &mut names, block, R11, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R11));

        assert_eq!(clean(&mut func, &moves, &mut names), Cleaned { gone: 0, copied: 1 });
        assert_eq!(written(&func, block, &names), ["x64.mov_mr_64", "x64.mov_rr_64"]);
        assert_eq!(reads(&func, block, 1), vec![R10]);
    }

    /// The same reload with nothing having put the word in a register. There is nowhere to read it
    /// from but the frame, so the load is the instruction it was.
    #[test]
    fn a_reload_no_register_holds_the_word_of_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let reload = load(&mut func, &mut names, block, R11, 16);
        let mut moves = Moves::default();
        moves.record(reload, back(block, 0, R11));

        assert_eq!(clean(&mut func, &moves, &mut names), Cleaned::default());
        assert_eq!(written(&func, block, &names), ["x64.mov_rm_64"]);
    }

    /// Three registers holding one word is three right answers, and the pass takes the lowest
    /// numbered of them every time rather than whichever the map hands back first.
    #[test]
    fn the_copy_is_written_out_of_the_lowest_numbered_register_that_has_the_word() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let first = copy(&mut func, &mut names, block, R11, R10);
        let second = copy(&mut func, &mut names, block, RAX, R10);
        let reload = load(&mut func, &mut names, block, PhysReg::new(12), 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(first, across(block, R11, R10));
        moves.record(second, across(block, RAX, R10));
        moves.record(reload, back(block, 0, PhysReg::new(12)));

        assert_eq!(clean(&mut func, &moves, &mut names), Cleaned { gone: 0, copied: 1 });
        assert_eq!(reads(&func, block, 3), vec![RAX], "rax is register zero");
    }

    /// A call takes the registers with it, so the word is in the frame and nowhere else and the
    /// reload behind one is a load rather than a copy of a register that no longer has it.
    #[test]
    fn a_reload_behind_a_call_is_not_written_as_a_copy() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let call = op(&mut names, "call");
        func.build(block, call).uses(Reg::physical(RAX), GPR).finish();
        let reload = load(&mut func, &mut names, block, R11, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R11));

        assert_eq!(clean(&mut func, &moves, &mut names), Cleaned::default());
        assert_eq!(written(&func, block, &names), ["x64.mov_mr_64", "x64.call", "x64.mov_rm_64"]);
    }

    /// A word written out to the frame has to go to the frame, so a store is left alone however
    /// many registers hold what it is storing.
    #[test]
    fn a_spill_of_a_word_another_register_holds_stays_a_store() {
        let (mut names, mut func, block) = empty();
        let copied = copy(&mut func, &mut names, block, R11, R10);
        let spill = store(&mut func, &mut names, block, R11, 16);
        let mut moves = Moves::default();
        moves.record(copied, across(block, R11, R10));
        moves.record(spill, out(block, 0, R11));

        assert_eq!(clean(&mut func, &moves, &mut names), Cleaned::default());
        assert_eq!(written(&func, block, &names), ["x64.mov_rr_64", "x64.mov_mr_64"]);
    }

    /// A slot number is a slot number in the class that owns it, so a pair that agrees on
    /// everything but the class is two different words and two registers that share a number.
    #[test]
    fn a_reload_of_another_class_stays() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, Edit { class: XMM, ..back(block, 0, R10) });

        assert_eq!(gone(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 2);
    }

    /// The same two instructions, with nothing saying the allocator wrote them, which is what a
    /// write to a local variable and a read of it back look like.
    #[test]
    fn a_store_and_a_load_the_allocator_did_not_write_stay() {
        let (mut names, mut func, block) = empty();
        store(&mut func, &mut names, block, R10, 16);
        load(&mut func, &mut names, block, R10, 16);

        assert_eq!(gone(&mut func, &Moves::default(), &mut names), 0);
        assert_eq!(left(&func, block), 2);
    }

    /// The last instruction of one block and the first of another are not next to each other. What
    /// ran before the second block is whatever jumped to it, which is any block that names it and
    /// not the one the text happens to be under.
    #[test]
    fn a_reload_at_the_top_of_another_block_stays() {
        let (mut names, mut func, first) = empty();
        let second = func.create_block();
        let spill = store(&mut func, &mut names, first, R10, 16);
        let reload = load(&mut func, &mut names, second, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(first, 0, R10));
        moves.record(reload, back(first, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, second), 1);
    }

    /// A copy of the word somewhere else does not move the word, so the reload behind one is still
    /// a read of what the register holds.
    #[test]
    fn a_copy_out_of_the_register_between_the_two_does_not_stop_it() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let copied = copy(&mut func, &mut names, block, RAX, R10);
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(copied, across(block, RAX, R10));
        moves.record(reload, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 1);
        assert_eq!(left(&func, block), 2);
    }

    /// A call destroys the registers the convention does not preserve and says so with a
    /// definition of each, except for the ones its arguments arrived in, which it names as reads.
    /// So what a call leaves alone is not a question the operand vector answers and the whole map
    /// goes.
    #[test]
    fn a_reload_behind_a_call_stays() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let call = op(&mut names, "call");
        func.build(block, call).uses(Reg::physical(RAX), GPR).finish();
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 3);
    }

    /// A slot is an offset from the stack pointer, so an instruction that moves the pointer moves
    /// every slot, and what a register holds is no longer what is at the address the reload names.
    #[test]
    fn a_reload_behind_a_write_of_the_stack_pointer_stays() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let sub = op(&mut names, "sub_ri_64");
        func.build(block, sub)
            .def(Reg::physical(SYSV.stack_pointer), GPR)
            .uses(Reg::physical(SYSV.stack_pointer), GPR)
            .imm(32)
            .finish();
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 3);
    }

    /// An instruction the description does not name is one nothing is known about, including which
    /// registers it writes.
    #[test]
    fn a_reload_behind_an_instruction_the_target_does_not_have_stays() {
        let (mut names, mut func, block) = empty();
        let spill = store(&mut func, &mut names, block, R10, 16);
        let strange = op(&mut names, "nothing_of_that_name");
        func.build(block, strange).finish();
        let reload = load(&mut func, &mut names, block, R10, 16);
        let mut moves = Moves::default();
        moves.record(spill, out(block, 0, R10));
        moves.record(reload, back(block, 0, R10));

        assert_eq!(gone(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 3);
    }

    /// The copy a computed `goto` leaves in front of its jump once the allocator has given both of
    /// its ends one register. It is not one of the allocator's moves, and it goes anyway.
    #[test]
    fn a_copy_of_a_register_into_itself_goes_without_being_one_of_the_allocators_moves() {
        let (mut names, mut func, block) = empty();
        copy(&mut func, &mut names, block, RAX, RAX);
        let kept = add(&mut func, &mut names, block, RAX, RAX);

        assert_eq!(itself(&mut func, &FRAME, &mut names), 1);
        assert_eq!(func.insts(block).collect::<Vec<_>>(), vec![kept]);
    }

    /// The same for the other class, whose copy is a different instruction.
    #[test]
    fn a_copy_of_a_vector_register_into_itself_goes() {
        let (mut names, mut func, block) = empty();
        let movaps = op(&mut names, "movaps_rr");
        let xmm = Reg::physical(PhysReg::new(3));
        func.build(block, movaps).def(xmm, XMM).uses(xmm, XMM).finish();

        assert_eq!(itself(&mut func, &FRAME, &mut names), 1);
        assert_eq!(left(&func, block), 0);
    }

    /// A copy between two registers is a copy, and a narrower copy of a register into itself
    /// clears the top half of it, so neither is nothing.
    #[test]
    fn a_copy_between_two_registers_and_a_narrower_one_into_itself_stay() {
        let (mut names, mut func, block) = empty();
        copy(&mut func, &mut names, block, RAX, R10);
        let movl = op(&mut names, "mov_rr_32");
        func.build(block, movl).def(Reg::physical(RAX), GPR).uses(Reg::physical(RAX), GPR).finish();

        assert_eq!(itself(&mut func, &FRAME, &mut names), 0);
        assert_eq!(left(&func, block), 2);
    }

    /// The pass that reads a reload in the instruction that wanted it, with the two registers
    /// x86-64 holds back.
    fn reloads(func: &mut Func, moves: &Moves, names: &mut Interner) -> usize {
        super::reloads(func, moves, &MACHINE, (GPR, &[R10, R11]), names)
    }

    /// A comparison of two registers, which is what the selector writes for a loop's test.
    fn compare(
        func: &mut Func,
        names: &mut Interner,
        block: Block,
        first: PhysReg,
        second: PhysReg,
    ) -> Inst {
        let cmp = op(names, "cmp_set_l_32");
        func.build(block, cmp)
            .def(Reg::physical(PhysReg::new(1)), GPR)
            .uses(Reg::physical(first), GPR)
            .uses(Reg::physical(second), GPR)
            .finish()
    }

    /// Two-address addition of those two registers into the first.
    fn sum(
        func: &mut Func,
        names: &mut Interner,
        block: Block,
        to: PhysReg,
        from: PhysReg,
    ) -> Inst {
        let add = op(names, "add_rr_64");
        func.build(block, add)
            .operand(Operand::write(Reg::physical(to), GPR).with(Constraint::Reuse(1)))
            .uses(Reg::physical(to), GPR)
            .uses(Reg::physical(from), GPR)
            .finish()
    }

    /// The loop bound of tamnd/rucc#1994's deforming loop, read back into a scratch register for
    /// the comparison and read nowhere else.
    #[test]
    fn a_bound_read_back_for_a_comparison_is_read_out_of_the_frame_by_it() {
        let (mut names, mut func, block) = empty();
        let reload = load(&mut func, &mut names, block, R10, 40);
        let cmp = compare(&mut func, &mut names, block, R13, R10);
        let mut moves = Moves::default();
        moves.record(reload, back(block, 0, R10));

        assert_eq!(reloads(&mut func, &moves, &mut names), 1);
        assert_eq!(func.insts(block).collect::<Vec<_>>(), vec![cmp]);
        assert_eq!(func[cmp].opcode, op(&mut names, "cmp_set_l_rm_32"));
        assert_eq!(reads(&func, block, 0), vec![R13, RAX]);
        let mem = func[func[cmp].mem.expect("an address")];
        assert_eq!((mem.base, mem.disp), (Some(2), 40));
    }

    /// Read on the left the comparison asks the same question the other way round.
    #[test]
    fn a_reload_on_the_left_of_a_comparison_turns_the_question_over() {
        let (mut names, mut func, block) = empty();
        let reload = load(&mut func, &mut names, block, R10, 40);
        let cmp = compare(&mut func, &mut names, block, R10, R13);
        let mut moves = Moves::default();
        moves.record(reload, back(block, 0, R10));

        assert_eq!(reloads(&mut func, &moves, &mut names), 1);
        assert_eq!(func[cmp].opcode, op(&mut names, "cmp_set_g_rm_32"));
        assert_eq!(reads(&func, block, 0), vec![R13, RAX]);
    }

    /// The register is read again further down, so the word has to arrive in it.
    #[test]
    fn a_reload_read_again_after_the_instruction_stays() {
        let (mut names, mut func, block) = empty();
        let reload = load(&mut func, &mut names, block, R10, 40);
        compare(&mut func, &mut names, block, R13, R10);
        add(&mut func, &mut names, block, RAX, R10);
        let mut moves = Moves::default();
        moves.record(reload, back(block, 0, R10));

        assert_eq!(reloads(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 3);
    }

    /// A load into a register the allocator hands out may be live into the next block, which this
    /// cannot see, so it is left alone.
    #[test]
    fn a_reload_into_a_register_that_is_not_scratch_stays() {
        let (mut names, mut func, block) = empty();
        let reload = load(&mut func, &mut names, block, RBX, 40);
        compare(&mut func, &mut names, block, R13, RBX);
        let mut moves = Moves::default();
        moves.record(reload, back(block, 0, RBX));

        assert_eq!(reloads(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 2);
    }

    /// A copy of the allocator's between the two touches no memory and leaves the register alone,
    /// and a spill between them writes the frame the load would be read from later.
    #[test]
    fn a_copy_between_the_two_is_passed_and_a_spill_is_not() {
        let (mut names, mut func, block) = empty();
        let reload = load(&mut func, &mut names, block, R10, 40);
        let between = copy(&mut func, &mut names, block, RBX, R13);
        compare(&mut func, &mut names, block, R13, R10);
        let mut moves = Moves::default();
        moves.record(reload, back(block, 0, R10));
        moves.record(between, across(block, RBX, R13));
        assert_eq!(reloads(&mut func, &moves, &mut names), 1);
        assert_eq!(left(&func, block), 2);

        let (mut names, mut func, block) = empty();
        let reload = load(&mut func, &mut names, block, R10, 40);
        let spill = store(&mut func, &mut names, block, R13, 40);
        compare(&mut func, &mut names, block, R13, R10);
        let mut moves = Moves::default();
        moves.record(reload, back(block, 0, R10));
        moves.record(spill, out(block, 0, R13));
        assert_eq!(reloads(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 3);
    }

    /// An addition whose answer is tied to its first source takes the load as its second, and
    /// not as its first, where the answer would have to be in the register the load was.
    #[test]
    fn a_tied_answer_takes_the_load_only_on_the_side_it_is_not_tied_to() {
        let (mut names, mut func, block) = empty();
        let reload = load(&mut func, &mut names, block, R10, 40);
        let add = sum(&mut func, &mut names, block, R13, R10);
        let mut moves = Moves::default();
        moves.record(reload, back(block, 0, R10));
        assert_eq!(reloads(&mut func, &moves, &mut names), 1);
        assert_eq!(func[add].opcode, op(&mut names, "add_rm_64"));

        let (mut names, mut func, block) = empty();
        let reload = load(&mut func, &mut names, block, R10, 40);
        sum(&mut func, &mut names, block, R10, R13);
        let mut moves = Moves::default();
        moves.record(reload, back(block, 0, R10));
        assert_eq!(reloads(&mut func, &moves, &mut names), 0);
        assert_eq!(left(&func, block), 2);
    }

    fn forward(func: &mut Func, names: &Interner) -> usize {
        super::forward(func, &MACHINE, &FRAME, (GPR, &[R10, R11]), names)
    }

    #[test]
    fn a_copy_into_scratch_is_read_out_of_the_register_it_came_from() {
        let (mut names, mut func, block) = empty();
        copy(&mut func, &mut names, block, R10, RBX);
        add(&mut func, &mut names, block, RAX, R10);
        add(&mut func, &mut names, block, R13, R10);
        assert_eq!(forward(&mut func, &names), 1);
        assert_eq!(left(&func, block), 2);
        assert_eq!(reads(&func, block, 0), [RBX]);
        assert_eq!(reads(&func, block, 1), [RBX]);
    }

    #[test]
    fn a_copy_stays_when_its_source_is_written_before_the_last_read() {
        let (mut names, mut func, block) = empty();
        copy(&mut func, &mut names, block, R10, RBX);
        add(&mut func, &mut names, block, RBX, RAX);
        add(&mut func, &mut names, block, R13, R10);
        assert_eq!(forward(&mut func, &names), 0);
        assert_eq!(reads(&func, block, 2), [R10]);
    }

    #[test]
    fn a_copy_stays_when_a_reader_writes_the_scratch_register_too() {
        let (mut names, mut func, block) = empty();
        copy(&mut func, &mut names, block, R10, RBX);
        add(&mut func, &mut names, block, R10, R10);
        assert_eq!(forward(&mut func, &names), 0);
        assert_eq!(left(&func, block), 2);
    }
}
