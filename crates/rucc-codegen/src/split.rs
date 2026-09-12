//! Splitting critical edges, so that every edge that carries values has somewhere to put them.
//!
//! Design: `spec/10-backend.md` section 10.4.
//!
//! An edge carries values when the block it goes to takes parameters, and giving a parameter its
//! value is a move. The move has to happen on the edge and not before it or after it, because
//! before it is a block that goes somewhere else too and after it is a block that is arrived at
//! from somewhere else too, and in either case the move would run on a path it was not written
//! for. An edge out of a block with one successor can put its moves at the end of that block,
//! since every path through it takes the edge. An edge into a block with one predecessor can put
//! them at the start of that block, for the same reason the other way round. An edge that is
//! neither, which is what a critical edge is, has neither place, and the allocator says so:
//! `rucc_regalloc` asserts that it never sees one.
//!
//! So one is turned into two. A block with nothing in it goes on the edge, the arguments move on
//! to the second half, and both halves are now uncritical: the first goes to a block with one
//! predecessor and the second leaves a block with one successor. Which of the two the moves end
//! up in is the allocator's answer and not this one's, and either is correct.
//!
//! # What it leaves behind
//!
//! An empty block, which is a jump to the next thing unless the layout puts it where it falls
//! through. That is a cost, and it is why an edge with nothing to carry is left alone: there are
//! no moves to find a place for, so splitting it would buy a jump and nothing else.
//!
//! # The other edge with nowhere to put a move
//!
//! A computed `goto` leaves its block through a register, and the moves an edge out of it carries
//! would have to be written somewhere the jump has already gone past. So there is a second pass
//! here, [`indirect`], which takes the values off those edges and puts them in a block of their
//! own in front of each label. It runs first, and what it leaves behind is edges the splitting
//! below then has nothing to do about.
//!
//! [`pads`] is here for the same reason and not for a reason of its own: the blocks those labels
//! begin at are addresses an indirect branch arrives at, and a machine that checks the forward edge
//! wants a landing pad at every one of them. Which block an address names is settled by the pass
//! above, so the pad is written after it and not where the prologue's own pad is written.

use std::collections::HashMap;

use rucc_base::Interner;
use rucc_mir as mir;
use rucc_target::{BranchInsts, FrameInsts};

/// Splits every critical edge that carries values, and gives back how many it split.
///
/// Run after lowering and before allocation. Running it twice is running it once, because the
/// blocks it adds have one successor each and are never the source of a critical edge.
pub fn critical(func: &mut mir::Func) -> usize {
    let preds = preds(func);
    let blocks: Vec<mir::Block> = func.blocks().collect();
    let mut split = 0;
    for block in blocks {
        if func[block].succs.len() < 2 {
            continue;
        }
        for index in 0..func[block].succs.len() {
            let call = func[block].succs[index].clone();
            if call.args.is_empty() || preds[call.block.index()] < 2 {
                continue;
            }
            // The new block is at the end of the layout, which is where a block that is a jump
            // and nothing else does the least harm before the layout pass has an opinion.
            //
            // It runs exactly as often as the edge it sits on is taken, and both halves of that
            // edge are now that edge, which is why the weight is copied onto all three rather
            // than left at what a block nobody told anything runs. A block on a cold edge that
            // claimed to run once per call would be one the layout put in the middle of the hot
            // path.
            let weight = call.weight;
            let half = func.create_block();
            func.set_weight(half, weight);
            *func.succs_mut(half) = vec![call];
            func.succs_mut(block)[index] = mir::BlockCall::to(half).taken(weight);
            split += 1;
        }
    }
    split
}

/// Takes the values off every edge out of a computed `goto`, and gives back how many blocks it
/// made to hold them.
///
/// Run after lowering and before [`critical`], which then sees edges with nothing on them and
/// leaves them alone. Running it twice is running it once, for the reason the splitting above is:
/// the blocks it adds end in a jump rather than in a branch through a register.
///
/// # What is wrong with the edge it takes the values off
///
/// Every other edge in the function is out of a block whose last instruction the layout writes, so
/// an edge that is the only way out of its block can put its moves at the end of that block and
/// they land in front of the jump. A block that leaves through a register already ends in the jump
/// when the allocator runs, because where it goes is a value and a value is something selection
/// reads rather than something the layout knows. Moves at the end of that block would be written
/// after the jump, where nothing runs them, and moves in front of it would be written across the
/// register the jump reads, which the allocator believes is dead from the jump onwards and is free
/// to hand to one of the moves.
///
/// So the moves go somewhere else. Each label an indirect branch reaches gets a block in front of
/// it that carries the values, the branch goes to that block with nothing on the edge, and the
/// address the `&&label` produces is the address of that block rather than of the label's own. The
/// new block is arrived at one way and leaves one way, so its own edge has both of the places the
/// splitting above talks about and the allocator is content.
///
/// # One label, one address, and two branches that disagree
///
/// A label has one address, so two computed `goto`s that reach it both arrive at whatever block
/// that address names, and the values they carry are not the same values. One block in front of
/// the label cannot move two different sets of registers.
///
/// So they are made to agree first. Each parameter of the label gets a register of its own, every
/// branch writes that register in front of its jump, and the block in front of the label carries
/// those registers and nothing else. That is what gcc does about the same problem, which it calls
/// coalescing across an abnormal edge, done here rather than while the values are still the
/// optimizer's.
///
/// Writing them in front of the jump is safe, which is not obvious, since a branch that goes five
/// ways writes the registers of one of those ways on the path to all five. What makes it safe is
/// that nothing reads those registers except the block in front of the label, and the only way to
/// reach that block is an edge out of a branch, which writes them on the way. So a value written
/// here and not used is a value overwritten before anything looks, whichever way the jump went.
///
/// # Panics
///
/// Panics on a class of register the machine named no move for, which is a function carrying a
/// value of a kind the target never said how to copy, and on a branch that has lost the terminator
/// it was found by, which nothing between the finding and the use of it can do. Both are a target
/// description or a function that was built wrongly, and both are worth finding here rather than as
/// a value that arrives somewhere it was never written.
pub fn indirect(
    func: &mut mir::Func,
    branch: &BranchInsts,
    frame: &FrameInsts,
    names: &mut Interner,
) -> usize {
    let jump = mir::Opcode::new(names.intern(&format!("{}{}", branch.prefix, branch.indirect)));
    let branches: Vec<mir::Block> = func
        .blocks()
        .filter(|&block| func.terminator(block).is_some_and(|last| func[last].opcode == jump))
        .collect();
    // Nothing at all in almost every function, and the walk at the bottom is over every instruction
    // in it, so the answer is arrived at here rather than paid for everywhere.
    if branches.is_empty() {
        return 0;
    }
    // In the order the branches name them rather than in whatever order a hash gives, so that two
    // runs of the compiler over one program write the same blocks.
    let mut targets: Vec<mir::Block> = Vec::new();
    for &block in &branches {
        for call in &func[block].succs {
            if !call.args.is_empty() && !targets.contains(&call.block) {
                targets.push(call.block);
            }
        }
    }

    let mut entries: HashMap<mir::Block, mir::Block> = HashMap::new();
    for target in targets {
        let params = func[target].params.clone();
        let homes: Vec<mir::Reg> = params.iter().map(|param| func.new_vreg(param.class)).collect();
        let entry = func.create_block();
        let mut total = mir::Weight::NEVER;
        for &block in &branches {
            for index in 0..func[block].succs.len() {
                if func[block].succs[index].block != target {
                    continue;
                }
                let call = func[block].succs[index].clone();
                let last = func.terminator(block).expect("a block that ends in a jump");
                for (home, (arg, param)) in homes.iter().zip(call.args.iter().zip(&params)) {
                    let name = frame.moves(param.class).expect("a class this machine can move").mov;
                    let opcode = mir::Opcode::new(names.intern(&format!("{}{name}", frame.prefix)));
                    let inst = func
                        .build_loose(opcode)
                        .def(*home, param.class)
                        .uses(*arg, param.class)
                        .finish();
                    func.insert_before(last, inst);
                }
                // The block in front of the label runs as often as every branch that reaches it,
                // which is the same sum the weight of a block with that many edges into it would
                // be.
                total = mir::Weight::parts(total.raw().saturating_add(call.weight.raw()));
                func.succs_mut(block)[index] = mir::BlockCall::to(entry).taken(call.weight);
            }
        }
        func.set_weight(entry, total);
        *func.succs_mut(entry) = vec![mir::BlockCall::with(target, homes).taken(total)];
        entries.insert(target, entry);
    }

    // And the addresses, which is the half of this that is not about edges. Every `&&label` in the
    // function names a block, and a label with a block in front of it now begins at that block, so
    // an address left pointing at the label's own block would be a jump past the moves.
    let mut addresses: Vec<mir::MemRef> = Vec::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            if let Some(mem) = func[inst].mem {
                addresses.push(mem);
            }
        }
    }
    for mem in addresses {
        if let Some(named) = func[mem].block {
            if let Some(&entry) = entries.get(&named) {
                func[mem].block = Some(entry);
            }
        }
    }
    entries.len()
}

/// Puts a landing pad at the front of every block whose address is taken, and gives back how many
/// it wrote.
///
/// Run after [`indirect`], because the block an address names is not settled until that has moved
/// the addresses on to the blocks it made, and only when the command line asked for the forward
/// edge to be checked. Nothing is written otherwise, which is why the name comes in as an option
/// and why a target with no such instruction is a target this does nothing on.
///
/// The pad a prologue opens with is written elsewhere, in `crate::finish`, because the address it
/// makes reachable is the address of the function rather than a place inside it. These are the
/// other addresses an indirect branch may arrive at, and a machine that checks the forward edge
/// faults on one that has no pad, so a computed `goto` compiled without this would be a program
/// that ran everywhere except on the hardware the flag was turned on for.
pub fn pads(
    func: &mut mir::Func,
    frame: &FrameInsts,
    landing: Option<&'static str>,
    names: &mut Interner,
) -> usize {
    let Some(name) = landing else { return 0 };
    let opcode = mir::Opcode::new(names.intern(&format!("{}{name}", frame.prefix)));
    let mut addressed: Vec<mir::Block> = Vec::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            if let Some(mem) = func[inst].mem {
                if let Some(named) = func[mem].block {
                    if !addressed.contains(&named) {
                        addressed.push(named);
                    }
                }
            }
        }
    }
    for &block in &addressed {
        let inst = func.build_loose(opcode).finish();
        func.prepend_inst(block, inst);
    }
    addressed.len()
}

/// How many edges arrive at each block, counted by index rather than in layout order so that a
/// block added while splitting can be looked up in the same table.
fn preds(func: &mir::Func) -> Vec<usize> {
    let mut counts = vec![0; func.block_count()];
    for block in func.blocks() {
        for call in &func[block].succs {
            counts[call.block.index()] += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_target::x86_64::{BRANCH, FRAME, GPR, REGS};

    use super::*;

    /// A diamond: one block that goes two ways and one block both ways arrive at, with as many
    /// parameters on the block they arrive at as the test asks for.
    fn diamond(params: usize) -> (Interner, mir::Func, [mir::Block; 4]) {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));
        let head = func.create_block();
        let left = func.create_block();
        let right = func.create_block();
        let join = func.create_block();
        // The values arrive in the head, so that they have somewhere to be defined and the
        // printer has a name for them. Nothing here runs an allocator, which is the one thing
        // that would object to a first block with parameters.
        let args: Vec<mir::Reg> = (0..params).map(|_| func.append_param(head, GPR)).collect();
        for _ in 0..params {
            func.append_param(join, GPR);
        }
        *func.succs_mut(head) = vec![mir::BlockCall::to(left), mir::BlockCall::to(right)];
        *func.succs_mut(left) = vec![mir::BlockCall::with(join, args.clone())];
        *func.succs_mut(right) = vec![mir::BlockCall::with(join, args)];
        (names, func, [head, left, right, join])
    }

    /// Where each block goes, which is the whole of what this changes.
    fn edges(func: &mir::Func) -> Vec<Vec<usize>> {
        func.blocks()
            .map(|block| func[block].succs.iter().map(|call| call.block.index()).collect())
            .collect()
    }

    #[test]
    fn an_edge_that_is_the_only_way_out_is_left_alone() {
        let (_, mut func, _) = diamond(1);
        // The two edges into the join carry a value each and neither is critical, because the
        // block each leaves goes nowhere else.
        assert_eq!(critical(&mut func), 0);
        assert_eq!(edges(&func), vec![vec![1, 2], vec![3], vec![3], vec![]]);
    }

    #[test]
    fn a_critical_edge_carrying_a_value_is_split_in_two() {
        let (_, mut func, [head, _, _, join]) = diamond(1);
        // Now the head goes straight to the join as well, so both of its arms are critical: it
        // has two ways out and the join has three ways in.
        let arg = func.append_param(head, GPR);
        func.succs_mut(head).push(mir::BlockCall::with(join, vec![arg]));
        func.succs_mut(head).swap(1, 2);

        assert_eq!(critical(&mut func), 1);
        assert_eq!(
            edges(&func),
            // The head's second arm is the new block and the new block goes to the join. The
            // other two arms are untouched, because each goes to a block with one way in.
            vec![vec![1, 4, 2], vec![3], vec![3], vec![], vec![3]]
        );
    }

    #[test]
    fn a_critical_edge_carrying_nothing_is_left_alone() {
        let (_, mut func, [head, _, _, join]) = diamond(0);
        func.succs_mut(head).push(mir::BlockCall::to(join));

        // Critical and not split, because there is no move to find a place for and a block that
        // is a jump and nothing else is worth more than nothing.
        assert_eq!(critical(&mut func), 0);
    }

    #[test]
    fn the_arguments_move_on_to_the_half_that_arrives() {
        let (names, mut func, [head, _, _, join]) = diamond(1);
        let arg = func.append_param(head, GPR);
        func.succs_mut(head).push(mir::BlockCall::with(join, vec![arg]));

        assert_eq!(critical(&mut func), 1);
        // What the first half carries is nothing, since the block it goes to asks for nothing,
        // and what the second half carries is what the whole edge used to.
        let half = func.blocks().last().expect("the block the split added");
        assert_eq!(func[head].succs[2].args, Vec::new());
        assert_eq!(func[half].succs[0].args, vec![arg]);
        assert_eq!(
            mir::print_func(&func, &names, &REGS),
            "mfunc @f {\nblock0(%0:gpr, %1:gpr):\n    block1, block2, block4\n\n\
             block1:\n    block3(%0)\n\nblock2:\n    block3(%0)\n\n\
             block3(%2:gpr):\n\nblock4:\n    block3(%1)\n}\n"
        );
    }

    #[test]
    fn splitting_twice_is_splitting_once() {
        let (_, mut func, [head, _, _, join]) = diamond(1);
        let arg = func.append_param(head, GPR);
        func.succs_mut(head).push(mir::BlockCall::with(join, vec![arg]));

        assert_eq!(critical(&mut func), 1);
        assert_eq!(critical(&mut func), 0);
    }

    /// A function with one label whose address is taken and as many blocks leaving through that
    /// address as the test asks for, each carrying as many values to the label as it asks for.
    fn computed(branches: usize, params: usize) -> (Interner, mir::Func) {
        let mut names = Interner::new();
        let mut func = mir::Func::new(names.intern("f"));
        let head = func.create_block();
        let label = func.create_block();
        for _ in 0..params {
            func.append_param(label, GPR);
        }
        let lea = mir::Opcode::new(names.intern("x64.lea_64"));
        let jump = mir::Opcode::new(names.intern("x64.jmp_reg"));
        for _ in 0..branches {
            // Every branch works the address out for itself, which is what a program that takes
            // the address of a label twice looks like once the values are in registers.
            let args: Vec<mir::Reg> = (0..params).map(|_| func.append_param(head, GPR)).collect();
            let address = func.new_vreg(GPR);
            let at = if branches == 1 { head } else { func.create_block() };
            func.build(at, lea).def(address, GPR).mem(mir::Mem::block(label)).finish();
            func.build(at, jump).operand(mir::Operand::read(address, GPR)).finish();
            *func.succs_mut(at) = vec![mir::BlockCall::with(label, args)];
        }
        (names, func)
    }

    /// Which block each address in the function names, in the order the instructions are in.
    fn addressed(func: &mir::Func) -> Vec<usize> {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter_map(|inst| func[inst].mem)
            .filter_map(|mem| func[mem].block)
            .map(mir::Block::index)
            .collect()
    }

    #[test]
    fn the_values_a_computed_goto_carries_move_into_a_block_in_front_of_the_label() {
        let (mut names, mut func) = computed(1, 1);
        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &mut names), 1);

        // The branch goes to the new block carrying nothing, and the new block carries the value
        // the branch used to. The address the `lea` works out is the new block's as well, since
        // arriving at the label without going through the new block is arriving without the value.
        assert_eq!(edges(&func), vec![vec![2], vec![], vec![1]]);
        assert_eq!(func[mir::Block::new(0)].succs[0].args, Vec::new());
        assert_eq!(addressed(&func), vec![2]);
    }

    #[test]
    fn two_computed_gotos_that_reach_one_label_are_made_to_agree() {
        let (mut names, mut func) = computed(2, 1);
        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &mut names), 1);

        // One block in front of the label and not two, because the label has one address and both
        // branches arrive at it. What makes that sound is the move each branch writes in front of
        // its own jump, which puts its value in the register that block carries.
        assert_eq!(edges(&func), vec![vec![], vec![], vec![4], vec![4], vec![1]]);
        let text = mir::print_func(&func, &names, &REGS);
        assert_eq!(text.matches("x64.mov_rr_64").count(), 2, "{text}");
        // In front of the jump rather than behind it, since nothing behind a jump runs.
        for line in text.lines().collect::<Vec<_>>().windows(2) {
            if line[1].contains("x64.jmp_reg") {
                assert!(line[0].contains("x64.mov_rr_64"), "{text}");
            }
        }
        assert_eq!(addressed(&func), vec![4, 4]);
    }

    #[test]
    fn an_edge_out_of_a_computed_goto_that_carries_nothing_is_left_alone() {
        let (mut names, mut func) = computed(1, 0);

        // No values to carry, so no block to carry them, and the address stays the label's own.
        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &mut names), 0);
        assert_eq!(addressed(&func), vec![1]);
    }

    #[test]
    fn a_function_with_no_computed_goto_in_it_is_left_alone() {
        let (mut names, mut func, _) = diamond(1);
        assert_eq!(indirect(&mut func, &BRANCH, &FRAME, &mut names), 0);
        assert_eq!(edges(&func), vec![vec![1, 2], vec![3], vec![3], vec![]]);
    }

    #[test]
    fn what_it_leaves_is_nothing_for_the_splitting_below_to_do() {
        let (mut names, mut func) = computed(2, 1);
        indirect(&mut func, &BRANCH, &FRAME, &mut names);
        // The edges out of the branches carry nothing now, and the edges out of the blocks it
        // added are the only way out of those blocks, so neither kind is critical.
        assert_eq!(critical(&mut func), 0);
    }

    /// The first instruction of each block, by opcode, and an empty string for a block with
    /// nothing in it.
    fn opens(func: &mir::Func, names: &Interner) -> Vec<String> {
        func.blocks()
            .map(|block| match func.insts(block).next() {
                Some(inst) => names.resolve(func[inst].opcode.name()).to_owned(),
                None => String::new(),
            })
            .collect()
    }

    #[test]
    fn the_block_a_label_begins_at_gets_a_landing_pad_when_the_forward_edge_is_checked() {
        let (mut names, mut func) = computed(2, 1);
        indirect(&mut func, &BRANCH, &FRAME, &mut names);

        // One pad, at the block in front of the label, because that is the block both addresses
        // name once the values have been moved on to it. The label's own block is arrived at by an
        // ordinary edge from there and wants nothing.
        assert_eq!(pads(&mut func, &FRAME, FRAME.landing, &mut names), 1);
        assert_eq!(opens(&func, &names), ["", "", "x64.lea_64", "x64.lea_64", "x64.endbr64"]);
    }

    #[test]
    fn a_label_with_no_block_in_front_of_it_gets_the_pad_itself() {
        let (mut names, mut func) = computed(1, 0);
        indirect(&mut func, &BRANCH, &FRAME, &mut names);

        // Nothing was moved on to anything, so the address still names the label and the pad goes
        // where the address goes.
        assert_eq!(pads(&mut func, &FRAME, FRAME.landing, &mut names), 1);
        assert_eq!(opens(&func, &names), ["x64.lea_64", "x64.endbr64"]);
    }

    #[test]
    fn nothing_is_written_when_the_forward_edge_is_not_checked() {
        let (mut names, mut func) = computed(1, 0);
        assert_eq!(pads(&mut func, &FRAME, None, &mut names), 0);
        assert_eq!(opens(&func, &names), ["x64.lea_64", ""]);
    }
}
