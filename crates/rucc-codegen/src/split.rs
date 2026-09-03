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

use rucc_mir as mir;

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
            let half = func.create_block();
            *func.succs_mut(half) = vec![call];
            func.succs_mut(block)[index] = mir::BlockCall::to(half);
            split += 1;
        }
    }
    split
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
    use rucc_target::x86_64::{GPR, REGS};

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
        *func.succs_mut(left) = vec![mir::BlockCall { block: join, args: args.clone() }];
        *func.succs_mut(right) = vec![mir::BlockCall { block: join, args }];
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
        func.succs_mut(head).push(mir::BlockCall { block: join, args: vec![arg] });
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
        func.succs_mut(head).push(mir::BlockCall { block: join, args: vec![arg] });

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
        func.succs_mut(head).push(mir::BlockCall { block: join, args: vec![arg] });

        assert_eq!(critical(&mut func), 1);
        assert_eq!(critical(&mut func), 0);
    }
}
