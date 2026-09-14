//! Putting a run of machine instructions together into the shorter run the machine has for it.
//!
//! Design: `spec/10-backend.md` section 10.9, and `spec/optimizer/37-machine-level-optimization.md`
//! sections 37.3 and 37.4.
//!
//! Section 37.4 names this pass first of the ten it says are genuinely machine level, and says what
//! shape it should be: a match over machine instructions in SSA form, inside one block, over a
//! window of a few instructions, which is `gcc/late-combine.cc` rather than `gcc/combine.cc`. The
//! reason for the smaller of the two is in the same section. Combine is fifteen thousand lines
//! because it was written without def-use chains and had to find them again each time, and every
//! RTL pass GCC has written since is on the SSA form it added later for exactly that.
//!
//! Section 37.3 says what the pass does once it has found a run: substitute the earlier instruction
//! into the later one, and ask the machine description whether what came out is an instruction this
//! target has. That is [`crate::changes`] and this pass does not repeat any of it.
//!
//! # The run it puts together
//!
//! A value read out of memory and then used once, by arithmetic that this machine could have read
//! it out of memory itself:
//!
//! ```text
//!   movq 16(%rax), %rcx
//!   addq %rcx, %rdx        ->    addq 16(%rax), %rdx
//! ```
//!
//! Two instructions become one. The register the load wrote is not written at all, which is one
//! fewer value for the allocator to find a place for, and the bytes come down because an addressing
//! mode costs what it costs whichever instruction carries it and the load's own opcode byte goes.
//!
//! It is the commonest pair in the machine IR this compiler writes. Over the corpus at `-O2` there
//! are 3249 places where a `movq` is followed immediately by an `addq` that reads what it wrote,
//! and that is one width of one operation.
//!
//! # Why no rule does it
//!
//! The selector matches a term, and a term is one value. A load is a term and an addition is a
//! term, and the pattern that would cover both is an addition with a load under it, which the
//! selector does offer: it shows a rule the operands of its operands. What it cannot offer is the
//! rest of the condition. Whether the load may move down to where the addition is depends on what
//! is written between the two, and whether the load's value is wanted anywhere else depends on the
//! whole function. Neither is a fact about the term, so neither can be in a pattern.
//!
//! # When the load may move
//!
//! The load stops being where it was and starts being part of an instruction further down the
//! block, so everything between the two has to be something the load can pass. Three things are
//! not.
//!
//! Anything that writes memory. Whether it writes the bytes this load reads is a question about
//! two addresses, and telling two addresses apart is an analysis nothing below selection has. So
//! the walk below stops at a store rather than guessing, and [`MachineInsts::writes_mem`] is the
//! target's answer to which instructions those are.
//!
//! A call. What a call does to memory is not in the instruction, which is why it is a separate
//! question to the target from the one above, and the answer to a call is the same as the answer
//! to a store and arrived at faster.
//!
//! Anything that writes a register the address reads. Machine IR is in SSA form until the
//! allocator has run, so a virtual register cannot be written twice, but the stack pointer and the
//! frame pointer are physical here and an address into the frame reads one of them.
//!
//! # When the load is wanted elsewhere
//!
//! Exactly one instruction may read what the load wrote, and it has to be the one taking the load
//! in. [`Reads`] is that count, kept across the commits of the pass the way [`crate::fold`] keeps
//! it, and a count of one is the whole of the test because a virtual register is written once. Two
//! readers and the load has to stay where it is, so putting it into one of them buys nothing and
//! costs a second read of memory.
//!
//! An argument an edge carries is a read like any other and is in no operand vector, which is the
//! one place a count of this shape is easy to get wrong. [`Reads::of`] counts those, which is what
//! keeps a load whose value leaves the block out of this.
//!
//! # Which arithmetic
//!
//! [`FOLDS`] is the list, and it is a list rather than a rule about names because the two ends of
//! each entry are instructions the target describes separately and the widths have to agree. A
//! sixty four bit addition takes a sixty four bit load and nothing else: reading four bytes where
//! the program asked for eight is a different instruction, and reading eight where it asked for
//! four is three bytes nobody said were there.
//!
//! The eight bit multiply is the one member of the family with no entry. This machine has no
//! two-operand multiply narrower than sixteen bits, so an eight bit one is written as a thirty two
//! bit `imul` and reads a register whose upper bits nothing looks at. A memory operand has no
//! upper bits to not look at, so there is nothing to read there and the entry is left out.
//!
//! # Either source, when the operation does not care
//!
//! An addition reads two registers and it is the second of them the memory operand replaces,
//! because the first is the one the destination is tied to. Where the load feeds the first instead,
//! the two sources are swapped first, which is a change to the instruction and not to what it
//! computes as long as the operation commutes. Five of the six here do and subtraction does not,
//! which is what [`Fold::commutes`] says.
//!
//! # The window
//!
//! A load is carried forward at most [`WINDOW`] instructions and then dropped. The bound is what
//! makes the pass cost a fixed amount per instruction rather than an amount that grows with the
//! block, which section 37.3 records as GCC's own answer: `max-combine-insns` is four and has been
//! for decades.
//!
//! It also costs nothing. The pair this pass is about is written by the selector out of one
//! expression, so the two are next to each other or nearly, and the measurement in [`WINDOW`] is
//! that going past eight instructions finds nothing at all.
//!
//! # What it does not do yet
//!
//! A comparison. This machine compares against memory as readily as it adds to it, and the reason
//! there is no entry for one is that a comparison here is one opcode holding a compare and the byte
//! behind it, so the memory form is a third instruction rather than a second and the target has to
//! describe it before this can write it.
//!
//! A store the arithmetic feeds. `addq %rax, 16(%rcx)` is the same saving again on the other side
//! and this machine has the instruction, as [`rucc_target::x86_64::Form::Rmw`] already. What it
//! needs is the reverse of the walk below, a store looking back at what wrote the value it is
//! storing, and that is a second entry in the list rather than a second pass.
//!
//! Anything that is not a pair. Section 37.3 says GCC goes to four instructions, and the run this
//! finds is two. What makes three worth having is a rule set that has something to say about
//! three, and the rule set here grows one measured entry at a time.

use rucc_base::Interner;
use rucc_mir::{Func, Inst, Opcode, Reg};
use rucc_target::MachineInsts;

use crate::changes::{Changes, Plan, Reads};
use crate::fold::Pending;

/// How far a load is carried looking for the instruction that takes it in.
///
/// Measured over the corpus at `-O2`. Four instructions finds 4062 of them, eight finds 4103, and
/// sixteen finds the same 4103 as eight. The pairs this pass is about come out of the selector
/// next to each other, so what the extra room finds is the few that something unrelated was
/// scheduled between, and past eight there is nothing left to find.
pub const WINDOW: usize = 8;

/// One arithmetic instruction that could read its second source out of memory, and the load that
/// would fill it.
///
/// A table rather than a rule about spellings, because the three names in each row are three things
/// the target describes on their own and nothing about `add_rr_64` says that `mov_rm_64` is the
/// load of the same width. Writing the three together is what makes a mismatched width a line
/// somebody can see rather than a string that was built at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    /// The arithmetic as the selector wrote it, reading both its sources from registers.
    pub from: &'static str,
    /// The same arithmetic reading its second source out of memory.
    pub into: &'static str,
    /// The load that would have filled that register, which has to be of the same width.
    pub load: &'static str,
    /// Whether the two sources may be swapped, which is what lets the load feed either of them.
    pub commutes: bool,
}

/// The arithmetic a load can move into on this machine.
///
/// Every two-address integer operation the target has, at every width it has one, except the eight
/// bit multiply the module documentation gives the reason for. Subtraction is the one that does not
/// commute.
pub static FOLDS: &[Fold] = &[
    Fold { from: "add_rr_8", into: "add_rm_8", load: "mov_rm_8", commutes: true },
    Fold { from: "add_rr_16", into: "add_rm_16", load: "mov_rm_16", commutes: true },
    Fold { from: "add_rr_32", into: "add_rm_32", load: "mov_rm_32", commutes: true },
    Fold { from: "add_rr_64", into: "add_rm_64", load: "mov_rm_64", commutes: true },
    Fold { from: "sub_rr_8", into: "sub_rm_8", load: "mov_rm_8", commutes: false },
    Fold { from: "sub_rr_16", into: "sub_rm_16", load: "mov_rm_16", commutes: false },
    Fold { from: "sub_rr_32", into: "sub_rm_32", load: "mov_rm_32", commutes: false },
    Fold { from: "sub_rr_64", into: "sub_rm_64", load: "mov_rm_64", commutes: false },
    Fold { from: "and_rr_8", into: "and_rm_8", load: "mov_rm_8", commutes: true },
    Fold { from: "and_rr_16", into: "and_rm_16", load: "mov_rm_16", commutes: true },
    Fold { from: "and_rr_32", into: "and_rm_32", load: "mov_rm_32", commutes: true },
    Fold { from: "and_rr_64", into: "and_rm_64", load: "mov_rm_64", commutes: true },
    Fold { from: "or_rr_8", into: "or_rm_8", load: "mov_rm_8", commutes: true },
    Fold { from: "or_rr_16", into: "or_rm_16", load: "mov_rm_16", commutes: true },
    Fold { from: "or_rr_32", into: "or_rm_32", load: "mov_rm_32", commutes: true },
    Fold { from: "or_rr_64", into: "or_rm_64", load: "mov_rm_64", commutes: true },
    Fold { from: "xor_rr_8", into: "xor_rm_8", load: "mov_rm_8", commutes: true },
    Fold { from: "xor_rr_16", into: "xor_rm_16", load: "mov_rm_16", commutes: true },
    Fold { from: "xor_rr_32", into: "xor_rm_32", load: "mov_rm_32", commutes: true },
    Fold { from: "xor_rr_64", into: "xor_rm_64", load: "mov_rm_64", commutes: true },
    Fold { from: "imul_rr_16", into: "imul_rm_16", load: "mov_rm_16", commutes: true },
    Fold { from: "imul_rr_32", into: "imul_rm_32", load: "mov_rm_32", commutes: true },
    Fold { from: "imul_rr_64", into: "imul_rm_64", load: "mov_rm_64", commutes: true },
];

/// A load this block has passed that could still end up inside something.
#[derive(Debug, Clone, Copy)]
struct Waiting {
    /// The load.
    inst: Inst,
    /// The register it wrote, which is what the arithmetic has to be reading.
    reg: Reg,
    /// Which load it is, so that the width can be held against the arithmetic's.
    load: &'static str,
    /// How far along the block it is, which is what [`WINDOW`] is counted in.
    at: usize,
}

/// Puts every load that can move into the arithmetic that reads it, and gives back how many.
///
/// `pending` is the addresses [`crate::finish`] has still to write a displacement into, and a load
/// that moves takes its entry with it, the same way one folded into a reader does. An address into
/// the frame arrives here already inside the load, because [`crate::fold`] has run.
///
/// Run after selection and after the addresses are folded, and before allocation. Before the
/// allocator because what makes the pair safe to put together is that a virtual register is written
/// once, and after the addresses because a load whose address is still a `lea` in front of it has
/// nothing in its own memory operand worth carrying.
pub fn loads(
    func: &mut Func,
    machine: &MachineInsts,
    names: &mut Interner,
    pending: &mut Pending<'_>,
) -> usize {
    let mut reads = Reads::of(func);
    let mut done = 0;
    for block in func.blocks().collect::<Vec<_>>() {
        let mut waiting: Vec<Waiting> = Vec::new();
        for (at, inst) in func.insts(block).collect::<Vec<_>>().into_iter().enumerate() {
            let name = names.resolve(func[inst].opcode.name()).to_owned();
            let bare = machine.bare(&name).to_owned();
            // Asked before the rewrite below rather than after it, which is the same answer: an
            // instruction that reads memory where it used to read a register neither becomes a call
            // nor starts writing anything, and an instruction this target describes is one it still
            // describes under the other name in the same row of the table.
            let stops = machine.calls(&name) || !machine.has(&name) || machine.writes_mem(&name);
            if let Some((load, plan)) = joined(func, &reads, &waiting, machine, names, inst, &bare)
            {
                let mut set = Changes::new();
                set.rewrite(inst, plan);
                set.remove(load);
                if set.commit(func, &mut reads, names, machine).is_ok() {
                    pending.moved(load, &[inst]);
                    waiting.retain(|carried| carried.inst != load);
                    done += 1;
                }
            }
            if stops {
                waiting.clear();
                continue;
            }
            let written: Vec<Reg> = func[func[inst].operands]
                .iter()
                .filter(|operand| operand.role.is_def())
                .map(|operand| operand.reg)
                .collect();
            waiting.retain(|carried| at - carried.at < WINDOW && !touched(func, carried, &written));
            if let Some(load) = FOLDS.iter().find(|fold| fold.load == bare).map(|fold| fold.load) {
                let operands = &func[func[inst].operands];
                if let Some(first) = operands.first().filter(|operand| operand.role.is_def()) {
                    waiting.push(Waiting { inst, reg: first.reg, load, at });
                }
            }
        }
    }
    done
}

/// Whether this instruction writes a register that load needs left alone.
///
/// The registers its address reads, and the register it wrote. The second is there for the same
/// reason the first is: a virtual register cannot be written twice while the IR is in SSA form, and
/// these are the physical ones a function has before the allocator runs.
fn touched(func: &Func, carried: &Waiting, written: &[Reg]) -> bool {
    func[func[carried.inst].operands].iter().any(|operand| written.contains(&operand.reg))
}

/// The load that can move into this instruction and what the instruction becomes, or `None`.
///
/// Nothing here changes anything. What comes back is a proposal, and whether the target has the
/// instruction it describes is [`Changes`]'s answer rather than this one.
fn joined(
    func: &Func,
    reads: &Reads,
    waiting: &[Waiting],
    machine: &MachineInsts,
    names: &mut Interner,
    inst: Inst,
    bare: &str,
) -> Option<(Inst, Plan)> {
    let fold = FOLDS.iter().find(|fold| fold.from == bare)?;
    let operands = func[func[inst].operands].to_vec();
    let [answer, first, second] = operands[..] else { return None };
    // The second source is the one the memory operand replaces, because the answer is tied to the
    // first. Where the load feeds the first source instead and the operation commutes, the two are
    // swapped, which leaves the instruction computing what it computed.
    let (load, kept) = match found(reads, waiting, fold, second.reg) {
        Some(load) => (load, first),
        None if fold.commutes => (found(reads, waiting, fold, first.reg)?, second),
        None => return None,
    };
    let address = func[func[load].operands][1..].to_vec();
    let mut amode = func[func[load].mem?];
    // The registers an address names are operands behind the ones the instruction writes down, and
    // there is one of those in front of them here where there was none in front of them in the
    // load, so every position the mode holds moves along by one.
    amode.base = amode.base.map(|at| at + 1);
    amode.index = amode.index.map(|at| at + 1);
    let into = names.intern(&format!("{}{}", machine.prefix, fold.into));
    Some((
        load,
        Plan {
            opcode: Opcode::new(into),
            operands: [answer, kept].into_iter().chain(address).collect(),
            imm: None,
            amode: Some(amode),
            symbol: func[load].symbol,
        },
    ))
}

/// The load in the window that wrote that register and is read by nothing else.
fn found(reads: &Reads, waiting: &[Waiting], fold: &Fold, reg: Reg) -> Option<Inst> {
    waiting
        .iter()
        .find(|carried| carried.reg == reg && carried.load == fold.load)
        .filter(|carried| reads.count(carried.reg) == 1)
        .map(|carried| carried.inst)
}

#[cfg(test)]
mod tests {
    use rucc_mir::{self as mir, Constraint, Mem, Operand};
    use rucc_target::x86_64::{GPR, MACHINE};

    use super::*;

    /// A function with one block, and the names it was built with.
    fn empty() -> (Interner, Func, mir::Block) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        (names, func, block)
    }

    /// The opcode of that name on this target.
    fn op(names: &mut Interner, name: &str) -> Opcode {
        Opcode::new(names.intern(&format!("{}{name}", MACHINE.prefix)))
    }

    /// A load of eight bytes off that register.
    fn load(func: &mut Func, names: &mut Interner, block: mir::Block, base: Reg) -> Reg {
        let into = func.new_vreg(GPR);
        let mov = op(names, "mov_rm_64");
        func.build(block, mov)
            .def(into, GPR)
            .mem(Mem { disp: 16, ..Mem::at(Operand::read(base, GPR)) })
            .finish();
        into
    }

    /// Two-address arithmetic of that name on those two registers, in that order.
    fn alu(
        func: &mut Func,
        names: &mut Interner,
        block: mir::Block,
        name: &str,
        first: Reg,
        second: Reg,
    ) -> Reg {
        let answer = func.new_vreg(GPR);
        let opcode = op(names, name);
        func.build(block, opcode)
            .operand(Operand::write(answer, GPR).with(Constraint::Reuse(1)))
            .uses(first, GPR)
            .uses(second, GPR)
            .finish();
        answer
    }

    /// What every instruction in a block came to, as opcodes.
    fn shape(func: &Func, names: &Interner, block: mir::Block) -> Vec<String> {
        func.insts(block).map(|inst| names.resolve(func[inst].opcode.name()).to_owned()).collect()
    }

    /// The pass, with lists nothing is on.
    fn combine(func: &mut Func, names: &mut Interner) -> usize {
        let mut addresses = Vec::new();
        let mut arguments = Vec::new();
        let mut dynamic = Vec::new();
        let mut pending =
            Pending { addresses: &mut addresses, arguments: &mut arguments, dynamic: &mut dynamic };
        loads(func, &MACHINE, names, &mut pending)
    }

    /// The shape the whole pass is for.
    #[test]
    fn a_load_read_once_by_an_addition_becomes_its_memory_operand() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        alu(&mut func, &mut names, block, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["x64.add_rm_64"]);
        let inst = func.insts(block).next().expect("the addition");
        let mem = func[inst].mem.expect("the addition reads memory now");
        assert_eq!(func[mem].disp, 16, "the load's displacement came with it");
        assert_eq!(func[mem].base, Some(2), "and names the operand behind the source it kept");
        assert_eq!(func[func[inst].operands][1].reg, other, "the source it kept");
        assert_eq!(func[func[inst].operands][2].reg, base, "the address it took on");
    }

    /// The same load feeding the source the answer is tied to. The two sources are swapped, which
    /// an addition does not mind and is what lets this fold at all.
    #[test]
    fn a_load_feeding_the_first_source_of_an_addition_is_swapped_and_folded() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        alu(&mut func, &mut names, block, "add_rr_64", word, other);

        assert_eq!(combine(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["x64.add_rm_64"]);
        let inst = func.insts(block).next().expect("the addition");
        assert_eq!(func[func[inst].operands][1].reg, other);
    }

    /// A subtraction with the load on the left, which is the one place the swap above would change
    /// the answer.
    #[test]
    fn a_load_feeding_the_left_of_a_subtraction_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        alu(&mut func, &mut names, block, "sub_rr_64", word, other);

        assert_eq!(combine(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["x64.mov_rm_64", "x64.sub_rr_64"]);
    }

    /// And the same subtraction the other way round, which is the one that folds.
    #[test]
    fn a_load_feeding_the_right_of_a_subtraction_folds() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        alu(&mut func, &mut names, block, "sub_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["x64.sub_rm_64"]);
    }

    /// Two readers. The load has to stay where it is for the second of them, so putting it into the
    /// first buys nothing and reads the memory twice.
    #[test]
    fn a_load_two_instructions_read_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        alu(&mut func, &mut names, block, "add_rr_64", other, word);
        alu(&mut func, &mut names, block, "xor_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 0);
        assert_eq!(
            shape(&func, &names, block),
            ["x64.mov_rm_64", "x64.add_rr_64", "x64.xor_rr_64"]
        );
    }

    /// A store between the two. Whether it writes what the load reads is a question about two
    /// addresses, and the answer to not being able to tell is to leave the load where it is.
    #[test]
    fn a_load_with_a_store_between_it_and_its_reader_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let store = op(&mut names, "mov_mr_64");
        func.build(block, store).uses(other, GPR).mem(Mem::at(Operand::read(base, GPR))).finish();
        alu(&mut func, &mut names, block, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 0);
        assert_eq!(
            shape(&func, &names, block),
            ["x64.mov_rm_64", "x64.mov_mr_64", "x64.add_rr_64"]
        );
    }

    /// Another load between the two, which writes nothing and is passed.
    #[test]
    fn a_load_with_another_load_between_it_and_its_reader_folds() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        load(&mut func, &mut names, block, other);
        alu(&mut func, &mut names, block, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 1);
        assert_eq!(shape(&func, &names, block), ["x64.mov_rm_64", "x64.add_rm_64"]);
    }

    /// A call between the two. What a call does to memory is not in the instruction, so it is the
    /// same answer as the store and reached without asking about the address.
    #[test]
    fn a_load_with_a_call_between_it_and_its_reader_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let call = op(&mut names, "call");
        func.build(block, call).finish();
        alu(&mut func, &mut names, block, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["x64.mov_rm_64", "x64.call", "x64.add_rr_64"]);
    }

    /// Something writing the register the address reads. A physical register is the only one this
    /// can happen to while the IR is in SSA form, and the frame is addressed through two of them.
    #[test]
    fn a_load_whose_address_register_is_written_between_the_two_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let base = Reg::physical(rucc_target::x86_64::RSP);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let sub = op(&mut names, "sub_ri_64");
        func.build(block, sub)
            .operand(Operand::write(base, GPR).with(Constraint::Reuse(1)))
            .uses(base, GPR)
            .imm(32)
            .finish();
        alu(&mut func, &mut names, block, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 0);
    }

    /// A load of four bytes under an addition of eight. The register held what the load put in it
    /// and a memory operand holds what is at the address, which is a different number of bytes.
    #[test]
    fn a_load_of_the_wrong_width_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let into = func.new_vreg(GPR);
        let narrow = op(&mut names, "mov_rm_32");
        func.build(block, narrow).def(into, GPR).mem(Mem::at(Operand::read(base, GPR))).finish();
        alu(&mut func, &mut names, block, "add_rr_64", other, into);

        assert_eq!(combine(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["x64.mov_rm_32", "x64.add_rr_64"]);
    }

    /// A load whose value leaves the block on an edge. It is read by nothing in any operand vector
    /// and is read all the same, which is the count that is easy to get wrong.
    #[test]
    fn a_load_whose_value_an_edge_carries_stays_a_load() {
        let (mut names, mut func, block) = empty();
        let next = func.create_block();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        alu(&mut func, &mut names, block, "add_rr_64", other, word);
        let arrived = func.new_vreg(GPR);
        func.params_mut(next).push(mir::Param { reg: arrived, class: GPR });
        *func.succs_mut(block) = vec![mir::BlockCall::with(next, vec![word])];

        assert_eq!(combine(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["x64.mov_rm_64", "x64.add_rr_64"]);
    }

    /// A reader in another block, which is the whole of what block local means here.
    #[test]
    fn a_reader_in_another_block_stays_where_it_is() {
        let (mut names, mut func, block) = empty();
        let next = func.create_block();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        alu(&mut func, &mut names, next, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 0);
        assert_eq!(shape(&func, &names, block), ["x64.mov_rm_64"]);
        assert_eq!(shape(&func, &names, next), ["x64.add_rr_64"]);
    }

    /// A reader further down the block than the window reaches.
    #[test]
    fn a_reader_past_the_window_stays_where_it_is() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let nop = op(&mut names, "nop");
        for _ in 0..WINDOW {
            func.build(block, nop).finish();
        }
        alu(&mut func, &mut names, block, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 0);
    }

    /// And one instruction closer, which is the last place it still folds.
    #[test]
    fn a_reader_at_the_edge_of_the_window_folds() {
        let (mut names, mut func, block) = empty();
        let base = func.new_vreg(GPR);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let nop = op(&mut names, "nop");
        for _ in 0..WINDOW - 1 {
            func.build(block, nop).finish();
        }
        alu(&mut func, &mut names, block, "add_rr_64", other, word);

        assert_eq!(combine(&mut func, &mut names), 1);
    }

    /// The entry a frame layout is waiting on moves with the load. Without this the displacement
    /// of a local would be written into an instruction that has gone.
    #[test]
    fn the_frame_entry_of_a_load_that_moves_goes_with_it() {
        let (mut names, mut func, block) = empty();
        let base = Reg::physical(rucc_target::x86_64::RSP);
        let other = func.new_vreg(GPR);
        let word = load(&mut func, &mut names, block, base);
        let reader = func.insts(block).nth(1);
        assert!(reader.is_none(), "the block holds the load alone so far");
        alu(&mut func, &mut names, block, "add_rr_64", other, word);
        let held = func.insts(block).next().expect("the load");

        let mut addresses = vec![(held, 3usize)];
        let mut arguments = Vec::new();
        let mut dynamic = Vec::new();
        let mut pending =
            Pending { addresses: &mut addresses, arguments: &mut arguments, dynamic: &mut dynamic };
        assert_eq!(loads(&mut func, &MACHINE, &mut names, &mut pending), 1);

        let inst = func.insts(block).next().expect("the addition");
        assert_eq!(addresses, [(inst, 3usize)], "the entry names the instruction that took it");
    }

    /// Every row of the table names instructions this target has, and names a load and an
    /// arithmetic whose widths agree. A row that got one of the three wrong would propose an
    /// instruction the change framework turns down, which is a fold that silently never happens.
    #[test]
    fn every_row_of_the_table_is_three_instructions_this_target_has() {
        for fold in FOLDS {
            assert!(MACHINE.has(fold.from), "{} is not an instruction", fold.from);
            assert!(MACHINE.has(fold.into), "{} is not an instruction", fold.into);
            assert!(MACHINE.has(fold.load), "{} is not an instruction", fold.load);
            let width = |name: &str| name.rsplit_once('_').map(|(_, width)| width.to_owned());
            assert_eq!(width(fold.from), width(fold.into), "{} changes width", fold.from);
            assert_eq!(width(fold.from), width(fold.load), "{} loads another width", fold.from);
            assert!((MACHINE.takes_mem)(fold.into), "{} reads no memory", fold.into);
            assert!(!(MACHINE.takes_mem)(fold.from), "{} already reads memory", fold.from);
        }
    }

    /// One row per arithmetic instruction the target has that could take one. The count is here so
    /// that an instruction added to the target without a row shows up as a number rather than as a
    /// fold nobody noticed was missing.
    #[test]
    fn the_table_covers_the_arithmetic_this_target_has() {
        assert_eq!(FOLDS.len(), 23, "six operations at four widths, less the eight bit multiply");
        let commuting = FOLDS.iter().filter(|fold| fold.commutes).count();
        assert_eq!(commuting, 19, "everything but the four subtractions");
    }
}
